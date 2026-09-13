# cuc ([Clink](https://github.com/chrisant996/clink) [Usage](https://usage.jdx.dev/) Completions)

Generate [Clink](https://github.com/chrisant996/clink) argmatchers from a [Usage](https://usage.jdx.dev/spec/reference/) specification.

## Installation

1. Download the appropriate executable from [Releases](https://github.com/IMXEren/cuc/releases/latest).
2. Make these Lua modules available through `package.path` (for example from `!init.lua` or `.init.lua`):
   - [arghelper.lua](./modules/arghelper.lua)
   - [base64.lua](./modules/base64.lua)
3. Put the generated Lua file in a Clink scripts directory, or load it explicitly.

See [mise-clink](https://github.com/binyaminyblatt/mise-clink) for a complete integration.

## Generating completions

Read a specification from a file:

```pwsh
cuc generate usage.kdl --out usage.lua
```

Or from stdin:

```pwsh
mycli usage | cuc generate --out usage.lua
```

Use `--complete` when the specification contains runtime `complete run=...` entries or `mount` nodes:

```pwsh
mycli usage | cuc generate --complete --out usage.lua
```

Dynamic completion uses Bash. cuc locates Git Bash automatically, or accepts an explicit executable:

```pwsh
mycli usage | cuc generate --complete --shell C:\msys64\usr\bin\bash.exe --out usage.lua
```

The generated script embeds the paths to `cuc` and the selected shell, which must remain available when Clink requests completions. Without `--complete`, runtime completers and mounts are silently omitted; ordinary flags, arguments, and commands are still generated.

To generate at Clink startup instead of writing a file:

```lua
load(io.popen("mycli usage | C:\\path\\to\\cuc.exe generate --complete"):read("*a"))()
```

## Usage support

cuc parses specifications with the official `usage-lib` parser. This includes strict current syntax, `include`, and `flagset`/`use` resolution.

Completion-relevant support includes:

- commands, visible aliases, flags, global flags, inline flag arguments, positionals, choices, defaults, and variadic arguments;
- root and command-scoped `complete` entries for `run`, `file`, and `dir`;
- `default_subcommand` and `default_subcommand_flags`;
- `arg.double_dash` modes, mapped to the closest available Clink parser behavior;
- clauses, including clause flags and their active positionals;
- dynamic mounts, resolved in the completion-time working directory;
- sigil arguments with fixed choices or runtime `run` completion;
- `restart_token` argument parsing; and
- hidden commands, flags, and aliases being excluded from suggestions.

## Known limitations

These limitations are deliberate where Clink has no equivalent construct:

- `group` is ignored. Validation relationships such as `conflicts`, `requires`, `overrides`, `required_if`, and `subcommand_required` do not filter suggestions.
- A single-positional clause is represented as one optional variadic argument. Clink cannot model a repeatable multi-positional clause separated by a token, so such a clause is emitted as one non-repeating positional group.
- Mounts are never snapshotted while generating. They require `--complete`; otherwise they are ignored. Dynamic mounts expose the mounted root command names, but do not graft the mounted commands' nested flags or argument trees. Mounted names are emitted without descriptions because `:` may be part of a command name.
- `restart_token` uses a generated parser chain and supports three further restarts. Clink parser links cannot form the cycle needed for unbounded repetition; additional tokens fall back to ordinary matching.
- `%` sigils are shadowed by Clink's environment-variable match generator and therefore cannot be completed. Dynamic sigils currently require `complete run=...`; fixed `choices` work without `--complete`.
- Dynamic completion descriptions are used only to strip the description suffix; they are not displayed as Clink match descriptions.
- Other Usage completion types (for example `path`, `command`, and `command_args`) are currently ignored unless represented by a `run` completer.
- Validation- or execution-oriented properties such as `flag.count`, `delimiter`, `unknown_flags`, environment/config bindings, and `config` are not represented in generated completion behavior.
- Informational nodes and properties such as help variants, examples, version, author, license, repository, and source links are not emitted unless they supply command/flag descriptions already supported by Clink.
