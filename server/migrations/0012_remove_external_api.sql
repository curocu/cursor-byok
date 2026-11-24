-- Remove settings and credentials for the retired external API.
DELETE FROM service_settings WHERE setting_key = 'external_api';
