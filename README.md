# ream

Rust-native CLI for the Ream framework. Instant startup, no Node.js boot penalty.

## Install

```bash
npm install -g @c9up/ream-cli
```

## Commands

Run `ream --help` (or `ream <command> --help`) for the authoritative,
always up-to-date list. What the binary itself defines:

```bash
# Scaffolding
ream new my-app                       # create a new Ream project
ream template kitchen-sink            # install a project template from its upstream repo

# Development
ream dev                              # dev server + whatever builds the assets, under one Ctrl-C
ream build                            # assets, then TypeScript into dist/
ream start                            # run production (spawns node)
ream test                             # the suites declared in reamrc.ts
ream repl                             # a Node REPL with the application booted

# Code generation
ream make:controller order Order      # app/order/OrderController.ts
ream make:service order Payment       # app/order/PaymentService.ts
ream make:entity order OrderItem      # app/order/OrderItem.ts
ream make:validator order CreateOrder
ream make:module order Order          # entity + controller + validator + migration
ream make:provider Stripe
ream make:command app:provision       # commands/app-provision.ts, discovered automatically
ream make:middleware auth --stack router
ream make:event orderShipped
ream make:listener sendMail --event orderShipped
ream stubs:publish controller         # copy a make: template into stubs/make/ to edit it

# Packages
ream add @c9up/atlas                  # install a package AND run its configure() hook
ream configure @c9up/photon           # (re-)run a package's configure() hook

# Integrations / tooling
ream mcp install                      # manage the Ream MCP server registration (.mcp.json)
ream generate:key                     # a fresh APP_KEY into .env
ream inspect                          # routes, providers, container bindings
ream list                             # every command — this binary's and the app's

# Diagnostics
ream doctor                           # environment health checks
ream info                             # version info
```

Every `make:` command takes `--dry-run` (print the plan as JSON, write nothing)
and `--force` (overwrite what is there).

Anything else is dispatched to the application's console kernel with its flags
intact, so a package's commands and the app's own run the same way:

```bash
ream migrate                          # @c9up/ream, across every registered store
ream migration:run                    # @c9up/atlas
ream make:migration create_orders_table
ream schedule:list
ream provision --email you@example.com   # the app's own
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
