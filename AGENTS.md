# Repository Guidelines

## Project Structure & Module Organization

`src/` contains the Rust service. `app.rs` assembles the application and `bootstrap.rs` owns startup and shutdown. Business code lives under `src/addon/`; application-level audit, authorization consistency, and declarative schema assembly live under `src/infrastructure/`; settings and source precedence live under `src/config/`. Keep each custom Action in its own file under an `actions/` directory and use `mod.rs` only for assembly and shared declarations. Rust integration coverage lives in `tests/system_integration.rs`. `examples/frontend_demo/` provides the database-free backend used by browser tests.

The Quasar/Vue application is in `frontend/`. Place API clients and contracts in `frontend/src/api/` and `frontend/src/contracts/`, reusable UI in `components/`, page orchestration in `pages/` and `layouts/`, and Pinia state in `stores/`. Unit tests are colocated as `*.test.ts`; Playwright specs live in `frontend/e2e/`.

## Build, Test, and Development Commands

- `python scripts/run_ci.py quick`: run architecture checks, Rust formatting and library tests, plus frontend type checks and Vitest.
- `python scripts/run_ci.py full`: run the complete pre-push gate, including all Rust targets, Clippy with warnings denied, and the frontend build/lint suite.
- `cargo run`: start the API after copying `config.example.toml` to ignored `config.toml` and configuring MySQL, Redis, and a random token secret.
- `pnpm --dir frontend install --frozen-lockfile` and `pnpm --dir frontend dev`: install and run the frontend.
- `pnpm --dir frontend e2e`: run Playwright with the demo backend and dev server.

## Coding Style & Naming Conventions

Use Rust 2021 and standard `rustfmt` output (four-space indentation). Use `snake_case` for files/functions, `PascalCase` for Rust types and Vue components, and descriptive Action names such as `actions/register.rs`. Production Rust forbids `unsafe` and denies `unwrap()`/`expect()`. Preserve the repository's Chinese documentation/comment style. Frontend code must pass Prettier, ESLint, and `vue-tsc`; do not construct dynamic imports from backend-provided strings.

## Testing Guidelines

Add focused Rust `#[cfg(test)]` tests beside logic and regression tests for changed frontend contracts or state. Name Playwright files `*.spec.ts`. Real integration tests require a dedicated MySQL database ending in `_test` and Redis DB 15; run `python scripts/run_ci.py integration`. No numeric coverage threshold is enforced, but changed behavior should be covered.

## Commit & Pull Request Guidelines

Follow the existing Conventional Commit pattern: `feat(frontend): ...`, `fix(schema): ...`, or `refactor(org): ...`. Keep commits scoped and avoid checking in `config.toml` or credentials. PRs should explain behavior and verification, link the issue when applicable, call out schema/config changes, and include screenshots for visible frontend changes. Run the full gate before requesting review.
