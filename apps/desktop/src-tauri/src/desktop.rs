use std::{
    process::{Command, ExitCode},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

use axum::{
    extract::{Extension, Json},
    http::StatusCode,
    routing::post,
    Router,
};
use tauri::{
    async_runtime::JoinHandle, webview::Color, AppHandle, Manager, RunEvent, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_opener::OpenerExt;
use tokio_util::sync::CancellationToken;

#[cfg(dev)]
use cursor_server::config::ConsoleSource;
use cursor_server::{App, Config, Result};

#[cfg(not(dev))]
use crate::frontend;
use crate::startup::{self, StartupDiagnostics};
use crate::tray;

pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
const AUTOSTART_ARG: &str = "--autostart";

struct DesktopRuntime {
    shutdown: CancellationToken,
    server: Mutex<Option<JoinHandle<Result<()>>>>,
    exiting: AtomicBool,
    server_addr: std::net::SocketAddr,
}

#[tauri::command]
fn open_terminal_with_command(command: String) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = command;
        Command::new("open").args(["-a", "Terminal"]).status()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &command])
            .spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        const TERMINALS: &[(&str, &[&str])] = &[
            ("x-terminal-emulator", &["-e"]),
            ("gnome-terminal", &["--"]),
            ("konsole", &["-e"]),
            ("xfce4-terminal", &["--execute"]),
            ("alacritty", &["-e"]),
            ("kitty", &[]),
        ];
        let script = format!("{command}; exec bash");
        for (terminal, separator) in TERMINALS {
            let mut process = Command::new(terminal);
            process.args(*separator);
            process.arg("bash").arg("-c").arg(&script);
            match process.spawn() {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(tauri::Error::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no supported terminal emulator found",
        )))
    }
}

#[derive(serde::Deserialize)]
struct OpenExternalUrlRequest {
    url: String,
}

fn open_external_url(app: &AppHandle, url: &str) -> std::result::Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("only absolute HTTP and HTTPS URLs are allowed".into());
    }

    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|error| format!("failed to open URL: {error}"))
}

async fn open_external_url_handler(
    Extension(app): Extension<AppHandle>,
    Json(request): Json<OpenExternalUrlRequest>,
) -> std::result::Result<StatusCode, (StatusCode, String)> {
    open_external_url(&app, &request.url)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|error| (StatusCode::BAD_REQUEST, error))
}

fn desktop_api_router(app: AppHandle) -> Router {
    Router::new()
        .route(
            "/__byok-api__/api/desktop/open-external-url",
            post(open_external_url_handler),
        )
        .layer(Extension(app))
}

fn create_main_window(
    app: &AppHandle,
    address: std::net::SocketAddr,
) -> tauri::Result<WebviewWindow> {
    let url = format!("http://{address}/__byok-api__/")
        .parse()
        .expect("local frontend URL");
    let builder = WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("Cursor BYOK")
        .inner_size(820.0, 558.0)
        .min_inner_size(820.0, 558.0)
        .center()
        .background_color(Color(20, 20, 20, 255))
        .decorations(cfg!(target_os = "macos"))
        .shadow(true)
        .resizable(true)
        .visible(false);

    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);

    builder.build()
}

/// 复用已隐藏或最小化的主窗口;仅首次打开时创建 webview。
pub(crate) fn open_main_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        tracing::info!("restoring existing main window");
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    tracing::info!("creating main window");
    let address = app.state::<DesktopRuntime>().server_addr;
    let window = create_main_window(app, address)?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

pub fn run() -> ExitCode {
    let diagnostics = match StartupDiagnostics::initialize() {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            startup::report_logging_failure(error.as_ref());
            return ExitCode::FAILURE;
        }
    };
    #[cfg(unix)]
    {
        let open_file_limit = match crate::resource_limits::raise_open_file_limit() {
            Ok(limit) => limit,
            Err(error) => {
                diagnostics.report_fatal(&error);
                return ExitCode::FAILURE;
            }
        };
        tracing::info!(
            requested = crate::resource_limits::REQUESTED_OPEN_FILE_LIMIT,
            previous = open_file_limit.previous,
            effective = open_file_limit.effective,
            hard = open_file_limit.hard,
            "open file limit configured"
        );
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        architecture = std::env::consts::ARCH,
        log_directory = %diagnostics.log_directory().display(),
        "desktop starting"
    );

    let started_by_autostart = std::env::args_os().any(|arg| arg == AUTOSTART_ARG);

    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            open_terminal_with_command,
            crate::update::check_portable_update,
            crate::update::install_portable_update,
        ])
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == AUTOSTART_ARG) {
                let _ = open_main_window(app);
            }
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            app.handle().plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                Some(vec![AUTOSTART_ARG]),
            ))?;
            let config = {
                let mut config = Config::desktop()?;
                config.app_version = env!("CARGO_PKG_VERSION").into();
                config
            };
            #[cfg(dev)]
            let config = {
                let mut config = config;
                config.console = Some(ConsoleSource::Proxy(
                    "http://127.0.0.1:1420"
                        .parse()
                        .expect("Vite development URL"),
                ));
                config
            };
            let server = tauri::async_runtime::block_on(App::new(config))?
                .merge_router(desktop_api_router(app.handle().clone()));
            #[cfg(not(dev))]
            let server = server.merge_router(frontend::router(app.handle().clone()));
            let listener = tauri::async_runtime::block_on(server.bind())?;
            let address = listener.local_addr()?;
            tauri::async_runtime::block_on(server.harness().cleanup_stale_settings())?;
            let desktop_settings =
                tauri::async_runtime::block_on(server.store().desktop_settings())
                    .unwrap_or_default();
            #[cfg(target_os = "macos")]
            app.handle()
                .set_dock_visibility(desktop_settings.show_dock_icon)?;
            let shutdown = CancellationToken::new();
            let server_shutdown = shutdown.clone();
            let app_handle = app.handle().clone();
            let task = tauri::async_runtime::spawn(async move {
                let result = server.serve_on(listener, server_shutdown).await;
                if let Err(error) = &result {
                    tracing::error!(%error, "desktop server stopped unexpectedly");
                    app_handle.exit(1);
                }
                result
            });
            app.manage(DesktopRuntime {
                shutdown,
                server: Mutex::new(Some(task)),
                exiting: AtomicBool::new(false),
                server_addr: address,
            });
            if desktop_settings.silent_start && started_by_autostart {
                tracing::info!("silent autostart enabled; starting without the main window");
            } else {
                open_main_window(app.handle())?;
            }
            tray::create(app)?;
            crate::update::signal_ready_if_requested()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            diagnostics.report_fatal(&error);
            return ExitCode::FAILURE;
        }
    };

    app.run(handle_run_event);

    ExitCode::SUCCESS
}

fn handle_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW_LABEL => {
            if !app
                .state::<DesktopRuntime>()
                .exiting
                .load(Ordering::Acquire)
            {
                api.prevent_close();
                if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    match window.hide() {
                        Ok(()) => {
                            tracing::info!("main window hidden to tray; server continues running")
                        }
                        Err(error) => tracing::error!(%error, "failed to hide main window"),
                    }
                }
            }
        }
        // 无窗口(例如静默启动)不代表退出;仅显式退出时停止后台服务。
        RunEvent::ExitRequested { code, api, .. } => match code {
            None => api.prevent_exit(),
            Some(_) => {
                let runtime = app.state::<DesktopRuntime>();
                if !runtime.exiting.swap(true, Ordering::AcqRel) {
                    tracing::info!("explicit desktop exit requested; stopping server");
                    api.prevent_exit();
                    runtime.shutdown.cancel();
                    let server = runtime.server.lock().expect("server lock poisoned").take();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(server) = server {
                            match tokio::time::timeout(Duration::from_secs(11), server).await {
                                Ok(Ok(Ok(()))) => {}
                                Ok(Ok(Err(error))) => {
                                    tracing::error!(%error, "desktop server shutdown failed")
                                }
                                Ok(Err(error)) => {
                                    tracing::error!(%error, "desktop server task failed")
                                }
                                Err(_) => tracing::warn!("desktop server shutdown timed out"),
                            }
                        }
                        app.exit(0);
                    });
                }
            }
        },
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => {
            let _ = open_main_window(app);
        }
        _ => {}
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "native window state timed out"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    #[ignore = "creates a real Windows WebView; run explicitly in an interactive desktop session"]
    fn native_minimize_close_restore_and_explicit_exit() {
        let directory = tempfile::tempdir().unwrap();
        let data_directory = directory.path().to_owned();
        let mut context = tauri::generate_context!();
        context.config_mut().identifier = "dev.cursorbyok.window-lifecycle-test".into();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let shutdown = CancellationToken::new();
        let observed_shutdown = shutdown.clone();
        let app = tauri::Builder::default()
            .any_thread()
            .setup(move |app| {
                app.manage(DesktopRuntime {
                    shutdown,
                    server: Mutex::new(None),
                    exiting: AtomicBool::new(false),
                    server_addr: "127.0.0.1:9".parse().unwrap(),
                });
                WebviewWindowBuilder::new(
                    app,
                    MAIN_WINDOW_LABEL,
                    WebviewUrl::External("about:blank".parse().unwrap()),
                )
                .title("Cursor BYOK isolated lifecycle test")
                .data_directory(data_directory)
                .build()?;
                let app = app.handle().clone();
                std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let window = app.get_webview_window(MAIN_WINDOW_LABEL).unwrap();
                        window.minimize().unwrap();
                        wait_for(|| window.is_minimized().unwrap());
                        open_main_window(&app).unwrap();
                        wait_for(|| {
                            !window.is_minimized().unwrap() && window.is_visible().unwrap()
                        });
                        window.set_title("lifecycle marker").unwrap();
                        window.close().unwrap();
                        wait_for(|| !window.is_visible().unwrap());
                        let retained = app
                            .get_webview_window(MAIN_WINDOW_LABEL)
                            .expect("close must not destroy WebView");
                        assert_eq!(retained.title().unwrap(), "lifecycle marker");
                        assert!(!app.state::<DesktopRuntime>().shutdown.is_cancelled());
                        open_main_window(&app).unwrap();
                        wait_for(|| window.is_visible().unwrap());
                        assert_eq!(window.title().unwrap(), "lifecycle marker");
                    }));
                    result_tx.send(result.is_ok()).unwrap();
                    app.exit(0);
                });
                Ok(())
            })
            .build(context)
            .unwrap();
        app.run_return(handle_run_event);
        assert!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(
            observed_shutdown.is_cancelled(),
            "explicit exit must stop the server"
        );
    }
}
