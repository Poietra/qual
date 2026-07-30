# CLI reference

The executable and command are both named `qual`.

## Commands

| Command | Purpose |
| --- | --- |
| `qual check [PATH...]` | Run diagnostics and optional fixes |
| `qual explain RULE` | Print the full documentation for one rule |
| `qual rules` | List rule IDs, status, severity, and confidence |
| `qual config` | Print resolved configuration and enforcement status |
| `qual cost PATH` | Explain symbolic render cost per Scene |
| `qual coverage [PATH...]` | Report unresolved analysis frontiers |
| `qual static-facts [PATH...]` | Emit StaticFacts v0 JSON |
| `qual change-impact --before OLD --after NEW` | Emit conservative source-change impact |
| `qual source-bridge PATH --request REQUEST.json` | Validate bounded source-patch candidates |

Run `qual <COMMAND> --help` for the exhaustive option list shipped by your
installed version.

## Common `check` options

| Option | Purpose |
| --- | --- |
| `--select`, `--ignore` | Select rule IDs or families |
| `--min-confidence` | Minimum confidence to display |
| `--fail-level` | Lowest severity that produces exit code 1 |
| `--profile` | Select one configured render profile or `all` |
| `--renderer`, `--fps`, `--resolution` | Override render facts |
| `--format` | `rich`, `concise`, `full`, `json`, `sarif`, or `github` |
| `--fix`, `--unsafe-fixes` | Apply safe or explicitly unsafe edits |
| `--baseline`, `--write-baseline` | Compare with or create a baseline |
| `--statistics` | Print diagnostic counts |
| `--analysis-summary` | Print coverage after diagnostics |
| `--no-cache` | Disable analysis-cache filesystem activity |

## Stable automation contracts

- Exit `0`: no finding reaches the failure threshold.
- Exit `1`: at least one finding reaches the failure threshold.
- Exit `2`: command-line, configuration, input, or internal error.
- JSON and SARIF go to stdout; logging and analysis summaries stay separate.
- Output is deterministically sorted for the same source and semantic config.

For serialized field definitions, use the [JSON schema index](schemas.md).
