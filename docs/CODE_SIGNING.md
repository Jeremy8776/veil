# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

This applies to Veil's Windows release files: `veil-<version>-windows-x64.msi` and the
programs in it and in `veil-<version>-windows-x64-portable.zip` (`veil-app.exe`,
`veil-node.exe`, `veil-probe.exe`).

## How a release is signed

- Every signed file is built from this repository's source by
  [`.github/workflows/release.yml`](../.github/workflows/release.yml) on GitHub-hosted
  runners. Nothing built on a personal machine is signed.
- SignPath checks that the files came from that workflow run, and that each program names
  itself Veil at the release's version.
- Each signing request is approved by hand by an approver below, after reading the draft
  release.
- macOS and Linux files are not part of this. `SHA256SUMS.txt` lists every release file, and
  Veil's own release key signs that list for the app's updater
  ([TRANSPARENCY.md](TRANSPARENCY.md), "Installing an update").

## Team roles

| Role | Members |
|---|---|
| Committers and reviewers | [Jeremy8776](https://github.com/Jeremy8776) |
| Approvers | [Jeremy8776](https://github.com/Jeremy8776) |

Every member uses multi-factor authentication on GitHub and SignPath.

## Privacy

Veil sends nothing to its authors and has no built-in servers. What it sends, to whom, and
how to turn each part off is set out in [TRANSPARENCY.md](TRANSPARENCY.md) (sections 2 and
3): call traffic goes only to the people and servers in your call, and the update check asks
GitHub for the latest release once per start unless you turn it off.

## For maintainers: setting it up

Not done yet. Until it is, Windows files are unsigned and SmartScreen warns.

1. Apply at <https://signpath.org/apply> for the repository `Jeremy8776/veil`. It needs a
   published release (v0.1.1 is one), the OSI licences (MIT, Apache-2.0), this page linked
   from the README under "Code signing policy", and MFA on GitHub.
2. Once accepted, in SignPath: create the project, paste
   [`packaging/windows/signpath-artifact-configuration.xml`](../packaging/windows/signpath-artifact-configuration.xml)
   as its artifact configuration, link the GitHub repository as a trusted build system,
   and make a release signing policy with manual approval.
3. In GitHub, `release` environment: the secret `SIGNPATH_API_TOKEN` (a SignPath user with
   submitter rights) and the variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`,
   `SIGNPATH_SIGNING_POLICY_SLUG`. The Windows release job then waits up to an hour for the
   approval and puts the signed files in the release.

The publisher Windows shows is SignPath Foundation, not a person's name.
