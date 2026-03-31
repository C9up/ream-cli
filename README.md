# ream

Rust-native CLI for the Ream framework. Instant startup, no Node.js boot penalty.

## Install

```bash
npm install -g @c9up/ream-cli
```

## Commands

```bash
# Create a new project
ream new my-app

# Development
ream dev          # start with hot-reload
ream build        # compile TypeScript
ream start        # run production

# Code generation
ream make:controller order Order
ream make:service order Payment
ream make:entity order OrderItem
ream make:validator order CreateOrder
ream make:provider Stripe
ream make:migration create_orders_table

# Package management
ream configure @c9up/atlas
ream configure @c9up/tailwind
ream configure @c9up/photon

# Diagnostics
ream doctor
ream info
```

## Build from source

```bash
cargo build --release
# Binary at target/release/ream
```

## License

MIT
