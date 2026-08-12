# Cached DBT Default and Cranelift Removal Plan

1. Add failing contract tests for the default cached DBT geometry and the two
   active product DBT profiles.
2. Centralize default DBT constants, implement `Default`, and narrow ordinary
   product profile iteration while keeping reference backends explicitly usable.
3. Remove the Cranelift machine path, modules, tests, dependencies, and lockfile
   entries without changing the native DBT path.
4. Run formatting, focused tests, the full test suite, dependency inspection,
   and a release product benchmark.
5. Commit inline on `main` and record the slice on umbrella issue #498.

