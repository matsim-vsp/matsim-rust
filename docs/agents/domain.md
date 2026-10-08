# Domain docs

Use a single-context layout for this repository.

## Before exploring

- Read `CONTEXT.md` at the repository root if it exists.
- Read relevant decisions in `docs/adr/` if it exists.
- If either is absent, proceed without treating its absence as a problem. Create domain docs only when domain terms or decisions need to be recorded.

## File structure

```text
/
├── CONTEXT.md
└── docs/
    └── adr/
```

Use domain terms from `CONTEXT.md` in issues, proposals, and tests. If a needed term is missing, flag it as a possible glossary gap. Surface conflicts with relevant ADRs rather than silently overriding them.
