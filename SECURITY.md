# Security Policy

## Reporting a vulnerability

Email **support@daisylocal.app** with "SECURITY" in the subject, or use
GitHub's private vulnerability reporting on this repository. Please don't
open a public issue for security problems.

Include what you can: affected version + build SHA (Settings → About),
platform, and reproduction steps. We'll acknowledge within a few business
days and keep you posted through the fix.

## Scope

Daisy is a local-first desktop app. Reports we especially care about:

- Anything that causes recording audio, transcripts, or summaries to leave
  the machine other than through a user-configured provider or integration
- Vault/encryption weaknesses (key handling, at-rest protection)
- Path traversal or arbitrary-file access through the IPC surface
- License or update-channel tampering that affects other users

Self-built binaries are in scope for code-level issues; the signed official
builds are the supported artifacts.

## Supported versions

The latest release. Fixes ship as regular updates; we don't backport.
