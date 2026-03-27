# Contributing to agent-browser

Thank you for your interest in contributing to agent-browser! This document provides guidelines and instructions for contributing.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Development Setup](#development-setup)
- [Coding Style](#coding-style)
- [Submitting Changes](#submitting-changes)
- [Project Structure](#project-structure)

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](https://www.contributor-covenant.org/version/2/1/code_of_conduct/). By participating, you are expected to uphold this code. Please report unacceptable behavior to the project maintainers.

## Development Setup

### Prerequisites

- **Node.js** 20.x or 22.x
- **Rust** (latest stable) - [Install via rustup](https://rustup.rs/)
- **pnpm** (recommended) - `npm install -g pnpm`

### Installation

1. **Fork and clone the repository:**
   ```bash
   git clone https://github.com/YOUR_USERNAME/agent-browser.git
   cd agent-browser
   ```

2. **Install dependencies:**
   ```bash
   pnpm install
   ```

3. **Build the project:**
   ```bash
   pnpm build
   pnpm build:native  # Build Rust CLI
   ```

4. **Link globally for testing:**
   ```bash
   pnpm link --global
   agent-browser install  # Download Chromium
   ```

### Running Tests

```bash
# Run all tests
pnpm test

# Run specific test file
pnpm test src/__tests__/browser.test.ts

# Run with coverage
pnpm test --coverage
```

### Development Workflow

1. **Create a feature branch:**
   ```bash
   git checkout -b feature/my-feature
   ```

2. **Make your changes and test:**
   ```bash
   pnpm test
   agent-browser open example.com  # Manual testing
   ```

3. **Build and verify:**
   ```bash
   pnpm build
   pnpm build:native
   ```

## Coding Style

### TypeScript

- **Strict mode**: Always enable `strict: true` in `tsconfig.json`
- **Formatting**: Use Prettier (configuration in `.prettierrc`)
- **Linting**: Use ESLint (configuration in `.eslintrc.js`)

**Key conventions:**
- Use `const` by default, `let` only when reassignment is needed
- Prefer `async/await` over `.then()` chains
- Use meaningful variable and function names
- Add JSDoc comments for public APIs
- Keep functions small and focused

### Rust

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Run `cargo fmt` before committing
- Run `cargo clippy` and fix all warnings

### Commit Messages

Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification:

```
type(scope): description

[optional body]

[optional footer]
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, semicolons)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Build process or auxiliary tool changes

**Examples:**
```
feat(cli): add --timeout flag to open command
fix(browser): handle navigation timeouts gracefully
docs(readme): update installation instructions
test(snapshot): add tests for ref resolution
```

## Submitting Changes

### Pull Request Process

1. **Ensure all tests pass:**
   ```bash
   pnpm test
   pnpm build
   pnpm build:native
   ```

2. **Update documentation:**
   - Update `README.md` if adding new commands or options
   - Add JSDoc comments for new functions
   - Update `CHANGELOG.md` if applicable

3. **Create a pull request:**
   - Go to [GitHub Pull Requests](https://github.com/vercel-labs/agent-browser/pulls)
   - Click "New Pull Request"
   - Select your feature branch
   - Fill in the PR template

4. **PR Requirements:**
   - All CI checks must pass
   - At least one approval from a maintainer
   - No merge conflicts
   - Follows coding style guidelines

### CI Checks

The following checks run on every PR:
- **Version Sync**: Ensure package versions are in sync
- **TypeScript**: Type checking and unit tests
- **Rust**: Compilation and clippy checks
- **Integration Tests**: End-to-end browser automation tests

## Project Structure

```
agent-browser/
├── src/                    # TypeScript source code
│   ├── cli.ts             # CLI entry point
│   ├── browser-manager.ts # Core browser management
│   └── __tests__/         # Unit tests
├── rust/                   # Rust CLI implementation
│   ├── src/
│   │   └── main.rs        # Rust CLI entry
│   └── Cargo.toml
├── scripts/                # Build and utility scripts
├── .github/workflows/      # GitHub Actions CI
├── package.json
├── tsconfig.json
└── README.md
```

## Getting Help

- **Documentation**: Check the [README](./README.md) for detailed usage
- **Issues**: Search [existing issues](https://github.com/vercel-labs/agent-browser/issues) or open a new one
- **Discussions**: Join the conversation in [GitHub Discussions](https://github.com/vercel-labs/agent-browser/discussions)

## License

By contributing, you agree that your contributions will be licensed under the [Apache-2.0 License](./LICENSE).

---

Thank you for contributing to agent-browser! 🎉
