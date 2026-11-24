-- Keep the applied migration history valid when removing the 1.0.9 feature.
DROP TABLE cursor_footer_runs;
DELETE FROM service_settings
WHERE setting_key IN ('cursor_reply_footer', 'cursor_reply_footer_token');
