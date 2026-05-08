# Vocabulary

Authoritative list of names the harness understands. Two layers:

1. **Harness-domain vocabulary** — names *the harness owns*. Drivers
   reference them in op templates. The harness provides their values.
2. **Per-driver vocabulary** — names *each driver owns* (volume_params
   shape, op-name, scenario field names, etc.). The harness has zero
   knowledge of these; it just plumbs whatever data the driver puts
   in `test-matrix.json` through to op-template substitution.

This doc covers (1). Per-driver vocabularies live in each consumer's
own README. There's a section at the bottom on the **translation-table
convention** that consumers should follow when they pick names.

---

## Harness-domain substitution tokens

These tokens, in `{...}` form, are recognised by the runner's
templating engine. They expand the same way in `[ops.<name>] command`
templates and in built-in op fields (`ship-to-vm` / `ship-to-host`'s
`src` / `dest`).

### v1 + v2 — flat tokens

| Token | Source | Notes |
|---|---|---|
| `{binary}` | `[project] binary`, resolved against consumer root | Absolute path, canonical, Windows extended-path prefix stripped. v2 falls back to the `.exe`-stripped path on non-Windows hosts. |
| `{tools.<name>}` | `[tools] <name>` | E.g. `{tools.fsck}` → the resolved fsck command. |
| `{image}` (v1 only) | per-scenario `image`, resolved against `[vm].image_dir` | v2 prefers `{scenario.image}`. |
| `{drive}` | runtime, picked just before mount | v1 `[mount]` flow only. |
| `{path}` / `{from}` / `{to}` / `{content}` / `{extra}` | per-op | v1 only. v2 uses `{step.path}` / `{step.from}` / etc. instead. |

### v2 only — dotted-path tokens

The hierarchical surface lets recipe steps reference scenario-level
data without the harness having to enumerate fields up-front.

| Token shape | What it resolves to |
|---|---|
| `{scenario.<dotted.path>}` | A field on the enclosing scenario's JSON. E.g. `{scenario.volume_params.label}` → the scenario's `volume_params.label`. Walks objects via key, arrays via numeric index. |
| `{step.<dotted.path>}` | A field on the *current* recipe step's JSON. E.g. `{step.path}` for `{ "op": "ls", "path": "/" }`. Cleared between steps. |
| `{<token>?}` | Same as the un-suffixed form, but missing → empty string instead of an error marker. Use for fields that scenarios may or may not fill. |

**Lookup order**: flat token first (so `{tools.fsck}` resolves even
though it has a dot), then dotted-path. Flat shadows dotted; the v1
vocabulary is small and stable, so collisions don't happen in
practice.

### v2 only — `when` predicate vocabulary

`[ops.<name>] when = "<dotted-path>"` evaluates the given path against
the same scenario / step namespace as `expand`. JSON truthiness:
`null` / `false` / `0` / `""` / `[]` / `{}` are false; everything else
is true. Empty / absent predicate is always true.

```toml
# only run when the scenario carries a `fixtures` field
[ops.write-fixtures]
host = "host"
when = "scenario.fixtures"
command = "{binary} write-fixtures {scenario.image}"
```

### v2 only — built-in transition ops

These op-names are recognised by the runner without an entry in
`harness.toml [ops]`:

| Op | Required step fields | Effect |
|---|---|---|
| `ship-to-vm`   | `src` (host path), `dest` (vm path) | scp src vm:dest |
| `ship-to-host` | `src` (vm path), `dest` (host path) | scp vm:src dest |

`src` and `dest` go through the same `{...}` substitution as command
templates. `{scenario.image}` is the typical value.

---

## Per-driver vocabulary — the translation-table convention

### Why drivers should converge on names

Concrete: NTFS calls a volume's smallest addressable unit
`cluster_size`; ext4 calls it `block_size`; exFAT calls it
`cluster_size`; FAT32 calls it `cluster_size`. Same concept, four
names. If every driver picks its own, cross-driver tooling (matrix
diff viewers, scenario-shape linters, schema validators) has to know
each driver's quirks.

The fix: drivers agree on a generic harness-level name (e.g.
`alloc_unit_size`) and document the translation in their README so
consumers landing on a specific driver can find their native term.

### Two reasons drivers should follow this

1. **Cross-driver tooling** that operates on `test-matrix.json` (a
   matrix viewer, a scenario-shape linter) only has to know the
   harness vocabulary, not N driver vocabularies.
2. **New drivers** (HFS+, exFAT, APFS, ...) inherit field names from
   existing drivers. Stops every new driver from re-debating
   `cluster_size` vs `block_size` vs `allocation_unit`.

### Per-driver README — recommended section shape

Every consumer's README should carry a "Scenario field translations"
section that tells someone landing on the driver what each native
concept is called inside this driver's `test-matrix.json`:

```markdown
## Scenario field translations (NTFS ↔ harness)

The test matrix uses harness-level generic field names rather than
NTFS-native terminology, so the same scenario shape works across
fs-* drivers. If you came here looking for a NTFS-native name and
can't find it, this table is where to look:

| NTFS name      | Harness name        | Notes |
|----------------|---------------------|-------|
| cluster_size   | alloc_unit_size     | Same concept, generic name. Valid: 512, 1024, 2048, 4096, ..., 65536. |
| MFT records    | (n/a)               | NTFS-specific; not modelled at scenario level. |
| chkdsk verdict | verdict_shape       | Mapped to clean / repair-ok / repair-required. |
```

### Adding a new generic name to this doc

Two rules to keep the harness vocabulary from bloating:

1. **A new generic name requires 2+ drivers using it.** Single-driver
   fields stay as driver-specific; they don't earn a row in the
   harness vocabulary. This is the bloat-prevention rule.
2. **Renaming a generic name is a breaking change.** Once two drivers
   have agreed on `alloc_unit_size`, they're committed. Encourages
   thought before naming.

When a new generic name is added: append a row to the cross-driver
table below, and add the corresponding rows to every existing
driver's README that uses it.

---

## Cross-driver vocabulary table

Names that 2+ existing drivers have agreed on. Driver-specific
fields don't appear here.

| Generic name | Concept | Drivers using it |
|---|---|---|
| `size_mib` | Volume size in mebibytes | (prospective) |
| `label` | Volume label | (prospective) |
| `alloc_unit_size` | Smallest addressable on-disk unit (cluster / block / sector) | (prospective: NTFS as `cluster_size`, ext4 as `block_size`) |
| `verdict_shape` | Pass/fail/repair contract for the scenario's verifier | (prospective) |
| `operations[].type` (v1) / `recipe[].op` (v2) | Generic op verb | all consumers |

Update this table when a name graduates from single-driver to
shared. Mark prospective entries until 2+ drivers are actually using
the field — that way the table stays an accurate snapshot of
what's been agreed in code, not aspirational.
