# Teac

A Rust-based compiler for the TeaLang (Teaching Programming Language), featuring LLVM IR generation and native AArch64 code generation.

## Features

- **Pest-based parser** with preprocessor support (`use` directives)
- **SSA-style intermediate representation** compatible with LLVM IR
- **Native AArch64 backend** with register allocation
- **Cross-platform testing** via Docker on macOS

## Quick Start

Build the compiler:

```bash
cargo build --release
```

Compile a TeaLang program to AArch64 assembly (the default `--emit` target):

```bash
cargo run -- tests/dfs/dfs.tea -o dfs.s
```

Stop earlier in the pipeline to inspect the AST or LLVM IR:

```bash
cargo run -- tests/dfs/dfs.tea --emit ast
cargo run -- tests/dfs/dfs.tea --emit ir -o dfs.ll
```

## Usage

```
teac [OPTIONS] <FILE>

Arguments:
  <FILE>  Input file (.tea source)

Options:
  --emit <EMIT>    Output target: ast, ir, or asm (default: asm)
  -o, --output <FILE>
                   Output file (default: stdout)
  -h, --help       Print help
```

### Examples

```bash
# Dump AST
cargo run -- program.tea --emit ast

# Generate LLVM IR
cargo run -- program.tea --emit ir -o program.ll

# Generate AArch64 assembly
cargo run -- program.tea --emit asm -o program.s
```

## Project Structure

```
src/
├── ast/            # Abstract Syntax Tree definitions
├── parser/         # Pest-driven AST construction (entry: parser.rs)
├── ir/             # Intermediate Representation & code generation
│   └── gen/        # IR generation from AST
├── opt/            # IR-level optimization passes (CFG, dominators, mem2reg)
├── asm/            # Assembly backends
│   ├── aarch64/    # AArch64 code generation & register allocation
│   └── common/     # Shared backend utilities (stack frames, layout)
├── experimental/   # Feature-gated experimental passes (return-type inference)
├── common/         # Cross-stage utilities (Pass trait, graph, target)
├── main.rs         # CLI entry point
└── tealang.pest    # Grammar definition
```

## Testing

Run the full test suite:

```bash
cargo test
```

### Platform Requirements

| Platform | Requirements |
|----------|--------------|
| **AArch64 Linux** | Native — just `gcc` |
| **x86/x86_64 Linux** | Cross-compiler + QEMU: `sudo apt install gcc-aarch64-linux-gnu qemu-user` |
| **macOS** | Docker Desktop (uses ARM64 Linux containers) |

## Resources

- [Pest Parser Repository](https://github.com/pest-parser/pest)
- [Pest Book (Documentation)](https://pest.rs/book/)