//! Tracks explicit run aborts separately from normal transport completion.
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct RunCancellations {
    runs: HashMap<String, Run>,
}

struct Run {
    token: CancellationToken,
    original_request_id: String,
    parent: Option<String>,
    root: Option<String>,
    aborted: bool,
    finished_at: Option<Instant>,
}

impl RunCancellations {
    pub fn token(&mut self, id: &str) -> CancellationToken {
        // Keep completed runs for the inference session's idle window so a
        // delayed child cannot escape a stop after its parent's SSE closes.
        self.runs.retain(|_, run| {
            run.finished_at
                .is_none_or(|at| at.elapsed() < Duration::from_secs(1800))
        });
        self.runs
            .entry(id.into())
            .or_insert_with(|| Run {
                token: CancellationToken::new(),
                original_request_id: id.into(),
                parent: None,
                root: None,
                aborted: false,
                finished_at: None,
            })
            .token
            .clone()
    }

    pub fn link_original(&mut self, id: &str, original: Option<&str>) {
        let Some(original) = original.filter(|value| !value.is_empty() && *value != id) else {
            return;
        };
        if let Some(run) = self.runs.get_mut(id) {
            if run.original_request_id == id {
                run.original_request_id = original.into();
                tracing::info!(
                    request_id = id,
                    original_request_id = original,
                    "linked BYOK inference retry"
                );
            }
        }
        if self.was_aborted(id) {
            self.abort(id);
        }
    }

    pub fn link(&mut self, id: &str, parent: Option<&str>, root: Option<&str>) {
        let parent = parent.filter(|value| !value.is_empty() && *value != id);
        let root = root.filter(|value| !value.is_empty() && *value != id);
        if let Some(run) = self.runs.get_mut(id) {
            let changed = (run.parent.is_none() && parent.is_some())
                || (run.root.is_none() && root.is_some());
            if run.parent.is_none() {
                run.parent = parent.map(str::to_owned);
            }
            if run.root.is_none() {
                run.root = root.map(str::to_owned);
            }
            if changed {
                tracing::info!(
                    request_id = id,
                    parent_request_id = run.parent.as_deref(),
                    root_parent_request_id = run.root.as_deref(),
                    "linked BYOK subagent cancellation"
                );
            }
        }
        let aborted = [parent, root]
            .into_iter()
            .flatten()
            .any(|ancestor| self.was_aborted(ancestor));
        if aborted {
            self.abort(id);
        }
    }

    pub fn abort(&mut self, id: &str) {
        let mut stopped = HashSet::from([id.to_owned()]);
        if let Some(run) = self.runs.get(id) {
            stopped.insert(run.original_request_id.clone());
        }
        loop {
            // Retries share one original ID; child headers can name either
            // that ID or an individual attempt. Expand both before cancelling.
            let linked: Vec<_> = self
                .runs
                .iter()
                .filter(|(key, run)| {
                    stopped.contains(*key)
                        || stopped.contains(&run.original_request_id)
                        || [run.parent.as_ref(), run.root.as_ref()]
                            .into_iter()
                            .flatten()
                            .any(|ancestor| stopped.contains(ancestor))
                })
                .flat_map(|(key, run)| [key.clone(), run.original_request_id.clone()])
                .collect();
            let previous_count = stopped.len();
            stopped.extend(linked);
            if stopped.len() == previous_count {
                break;
            }
        }
        for child in &stopped {
            if let Some(run) = self.runs.get_mut(child) {
                run.aborted = true;
                run.token.cancel();
            }
        }
        tracing::info!(
            request_id = id,
            descendant_count = stopped
                .iter()
                .filter(|key| self.runs.contains_key(*key))
                .count()
                .saturating_sub(1),
            "cancelled BYOK run and descendants"
        );
    }

    pub fn was_aborted(&self, id: &str) -> bool {
        let original = self
            .runs
            .get(id)
            .map_or(id, |run| run.original_request_id.as_str());
        self.runs
            .values()
            .any(|run| run.original_request_id == original && run.aborted)
    }

    pub fn finish(&mut self, id: &str) {
        if let Some(run) = self.runs.get_mut(id) {
            run.finished_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_parent_completion_and_transport_cleanup_do_not_abort_children() {
        let mut runs = RunCancellations::default();
        let parent = runs.token("parent");
        parent.cancel();
        runs.finish("parent");
        let child = runs.token("child");
        runs.link("child", Some("parent"), Some("parent"));
        assert!(!child.is_cancelled());
        let grandchild = runs.token("grandchild");
        runs.link("grandchild", Some("child"), Some("parent"));
        runs.abort("child");
        assert!(child.is_cancelled() && grandchild.is_cancelled());
        assert!(!runs.runs["parent"].aborted);
    }

    #[test]
    fn root_link_cancels_a_child_with_an_unavailable_intermediate_parent() {
        let mut runs = RunCancellations::default();
        runs.token("root");
        runs.abort("root");
        let child = runs.token("child");
        runs.link("child", Some("unavailable-parent"), Some("root"));
        assert!(child.is_cancelled());
    }

    #[test]
    fn stopping_one_child_keeps_parent_and_sibling_running() {
        let mut runs = RunCancellations::default();
        let parent = runs.token("parent");
        let child = runs.token("child");
        runs.link("child", Some("parent"), Some("parent"));
        let sibling = runs.token("sibling");
        runs.link("sibling", Some("parent"), Some("parent"));
        let grandchild = runs.token("grandchild");
        runs.link("grandchild", Some("child"), Some("parent"));
        runs.abort("child");
        assert!(child.is_cancelled() && grandchild.is_cancelled());
        assert!(runs.was_aborted("child") && runs.was_aborted("grandchild"));
        assert!(!parent.is_cancelled() && !sibling.is_cancelled());
        assert!(!runs.was_aborted("parent") && !runs.was_aborted("sibling"));
    }

    #[test]
    fn stopping_retry_cancels_original_attempt_and_children() {
        let mut runs = RunCancellations::default();
        let original = runs.token("original");
        original.cancel();
        runs.finish("original");
        let retry = runs.token("retry");
        runs.link_original("retry", Some("original"));
        assert!(!retry.is_cancelled());
        let child = runs.token("child");
        runs.link("child", Some("original"), Some("original"));
        let next_retry = runs.token("next-retry");
        runs.link_original("next-retry", Some("original"));
        let unrelated = runs.token("unrelated");
        runs.abort("next-retry");
        assert!(retry.is_cancelled() && next_retry.is_cancelled() && child.is_cancelled());
        assert!(runs.was_aborted("original"));
        assert!(!unrelated.is_cancelled());
    }

    #[test]
    fn late_retry_and_child_stay_cancelled_when_original_record_is_missing() {
        let mut runs = RunCancellations::default();
        runs.token("retry");
        runs.link_original("retry", Some("missing-original"));
        runs.abort("retry");
        let late = runs.token("late-retry");
        runs.link_original("late-retry", Some("missing-original"));
        let child = runs.token("child");
        runs.link("child", Some("missing-original"), Some("missing-original"));
        assert!(late.is_cancelled() && child.is_cancelled());
        assert!(runs.was_aborted("late-retry") && runs.was_aborted("child"));
    }

    #[test]
    fn expired_finished_records_are_pruned_without_losing_active_runs() {
        let mut runs = RunCancellations::default();
        runs.token("old");
        runs.runs.get_mut("old").unwrap().finished_at =
            Some(Instant::now() - Duration::from_secs(1801));
        runs.token("active");
        assert!(!runs.runs.contains_key("old"));
        assert!(runs.runs.contains_key("active"));
    }
}
