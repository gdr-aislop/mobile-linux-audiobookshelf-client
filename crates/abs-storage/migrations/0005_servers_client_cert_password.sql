-- PKCS#12 client-certificate bundles are opened with an export password; stored alongside the
-- bundle's path (`servers.client_cert_path`) so a configured mTLS server keeps working across
-- launches. Plaintext, like every other secret this database holds (account tokens) — the
-- threat model is local-cache durability, not defense against someone already reading the
-- profile directory.
ALTER TABLE servers ADD COLUMN client_cert_password TEXT;
