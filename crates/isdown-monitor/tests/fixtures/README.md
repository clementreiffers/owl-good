# GitLab Status.io contract

Fetched 2026-10-15 from https://status.gitlab.com/1.0/status/5b36dc6502d06804c08349f7 (HTTP 200). The public page uses Status.io; `/api/v2/summary.json` returns 404.

Inventory source: https://kb.status.io/developers/status-codes/ (verified 2026-10-15).

| Current status code | Label | Classification |
|---|---|---|
| 100 | Operational | Healthy |
| 200 | Maintenance / Under Maintenance / Planned Maintenance | Warning |
| 300 | Degraded Performance | Warning |
| 400 | Partial Service Disruption | Warning |
| 500 | Service Disruption | Failed |
| 600 | Security Event | Failed |

Codes 100/200/300/400 under incident progression mean Investigating/Identified/Monitoring/Resolved and MUST NOT be used as current service severity. Only `result.status_overall`, current `result.status`, and their current containers are classified. Historic incidents and planned maintenance records are excluded. Current maintenance is indicated by code 200. An unknown code or unmatched label is unknown. Regex tests are table driven and include all inventory entries, no match, priority, disabled rules and historical false positives.

The live page's chart also uses these aliases (verified in its JavaScript on the same date): `Degraded` (300), `Partial outage` (400), `Major outage` (500), `Critical` (600). The default expressions cover these aliases and tests exercise each one. Arbitrary future labels remain unknown. The GitLab adapter accepts only HTTPS status.gitlab.com URLs because its verified API endpoint is specific to that provider; use `statusio` with a direct JSON API URL or `regex` for other sites.
