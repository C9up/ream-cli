# ream

Rust-native CLI for the Ream framework. Instant startup, no Node.js boot penalty.

## Install

```bash
npm install -g @c9up/ream-cli
```

## Commands

Run `ream --help` (or `ream <command> --help`) for the authoritative, always
up-to-date list. The current surface:

```bash
# Scaffolding
ream new my-app                       # create a new Ream project
ream template kitchen-sink            # install a project template from its upstream repo

# Development
ream dev                              # dev server (node --watch + swc-node, DI metadata)
ream build                            # compile TypeScript to dist/
ream start                            # run production (spawns node)

# Code generation
ream make:controller order Order
ream make:service order Payment
ream make:entity order OrderItem
ream make:validator order CreateOrder
ream make:provider Stripe
ream make:migration create_orders_table
ream make:seeder OrdersSeeder
ream make:module order Order          # entity + controller + validator + migration
ream stubs:publish controller         # copy a make: template into stubs/make/ to edit it

# Database
ream migrate                          # run pending migrations
ream migrate:rollback                 # roll back the last batch
ream migrate:status                   # show migration status

# Packages
ream add @c9up/atlas                  # install a package AND run its configure() hook
ream configure @c9up/photon           # (re-)run a package's configure() hook

# Scheduling
ream schedule:list                    # list registered scheduled tasks
ream schedule:run <task>              # run one task now (admin override)

# Integrations / tooling
ream mcp                              # manage the Ream MCP server registration (.mcp.json)
ream nova:vapid:generate              # generate Web Push VAPID keys into .env
ream inspect                          # inspect routes, providers, decorated services

# Diagnostics
ream doctor                           # environment health checks
ream info                             # version info
```

## Build from source

```bash
cargo build --release
# Binary at target/release/ream
```

## Assets

`ream dev` runs the server and whatever builds your assets as one thing, and `ream build` builds the assets before TypeScript. Declare them in `reamrc.ts`:

```ts
export default {
  assets: {
    devServer: { command: 'pnpm', args: ['css:watch'] },
    build: { command: 'pnpm', args: ['css'] },
  },
}
```

Output is line-prefixed per process, and when one stops the other is stopped with it — a Ctrl-C leaves no orphan watcher holding the output file. Without this, an app ends up wiring `concurrently -k` itself.

Both keys are optional: with no `assets`, `ream dev` and `ream build` behave exactly as before.

## License

MIT
