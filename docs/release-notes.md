# Release Notes

## Unreleased

### Encrypted Agent Profile credentials

Agent Profile custom API URLs and API keys are now stored in `config.toml`
as one encrypted `api-credentials` value instead of plaintext
`api-base-url` and `api-key` fields. This protects the values from programs
that read the configuration file directly. The encryption key is embedded
in the NiumaTerm executable, so analyzing the executable or inspecting the
running agent process can still recover the values.

**One-way migration.** Profiles saved by older builds keep working: their
plaintext fields load normally and are replaced with the encrypted value on
the next settings save. After that save, older NiumaTerm builds can no
longer read the custom API URL or API key. To roll back to an older build,
delete the profile's `api-credentials` line from `config.toml` and re-enter
the custom URL and API key in that build's settings dialog.

If an `api-credentials` value is damaged or hand-edited, NiumaTerm reports
a startup configuration error naming the profile and leaves the file
untouched. Remove that line and re-enter the credentials to recover.
