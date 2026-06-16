---
"farthing": patch
---

Notify the farthing-web marketing site on published releases: a new `notify-web.yml` workflow sends a `repository_dispatch` carrying the release tag, so the site's version badge stays in sync. Requires a `FARTHING_WEB_DISPATCH_TOKEN` secret (see docs/release.md).
