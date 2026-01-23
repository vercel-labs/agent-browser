# Changesets

This project uses [Changesets](https://github.com/changesets/changesets) for versioning and changelog generation.

## Adding a changeset

When you make a change that should be released, run:

```bash
pnpm changeset
```

This will prompt you to:
1. Select the type of change (patch, minor, major)
2. Write a summary of your changes

The changeset file will be committed with your PR.

## Release process

When changesets are merged to `main`, the release workflow will:
1. Create a "Version Packages" PR that updates version numbers and changelogs
2. When that PR is merged, packages are automatically published to npm
