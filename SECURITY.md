# Security policy

## Reporting a vulnerability

Please report security problems **privately**, not in a public issue.

Use GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Include:

- what the problem is and what an attacker could do with it;
- the steps or a proof of concept to reproduce it;
- the GameLobbies version and Windows version you tested on.

Please don't include real API keys, tokens or personal data in a report. You'll get an acknowledgement as soon as the maintainer can, and credit in the release notes if you'd like it. Please give the maintainer a reasonable amount of time to release a fix before disclosing the issue publicly.

## Supported versions

Only the latest release receives security fixes. GameLobbies can update itself from **Settings → About and Updates**.

## What's in scope

Examples of issues we want to hear about:

- running an unintended program, or passing extra arguments to one, through launching, shortcuts, emulator profiles or scanned files;
- reading, changing or deleting files outside what the app needs;
- leaking credentials (SteamGridDB keys, IGDB/Twitch client secrets or other API keys) or other personal data;
- bypassing the update signature check or the download host allow-lists;
- code execution in the app's window, for example through themes, release notes or game metadata.

## How updates are protected

Updates are signed with a private key that is never stored in this repository. The app installs an update only when its signature matches the public key built into the app and is bound to the announced version; older versions are refused. See [docs/RELEASING.md](docs/RELEASING.md) for how releases are built and signed.
