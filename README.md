# cuc ([Clink](https://github.com/chrisant996/clink) [Usage](https://github.com/jdx/usage) Completions)

A CLI tool to generate clink argmatcher completions from the usage spec.

## Installation

1. Go to [Releases](https://github.com/IMXEren/cuc/releases/latest) and download the executable to a desired location. You could also use powershell to download

    ```pwsh
    Invoke-RestMethod "https://github.com/IMXEren/cuc/releases/download/v0.1.0/cuc-v0.1.0-x64.exe" -OutFile "cuc.exe"
    ```

2. The generated `usage.completions.lua` requires that you have these modules in your package.path (you can also use `!init.lua` or `.init.lua`, to ensure the modules are added to package.path):

    - [arghelper.lua](./modules/arghelper.lua)

3. For dynamic completion i.e. a usage.spec.kdl that uses `complete`, you'd need a shell while generating. The `complete` node in the spec uses run command that require unix shells. As a workaround, you can use git-bash which would work fine (CLI already uses it). So, you'd need to specify when using shell other than git-bash (or if not found) like MSYS2 environment.

4. For loading completions, you can either provide the spec from a file or by stdin.

    ```lua
    load(io.popen("abs/path/cuc.exe generate [OPTIONS] abs/path/usage.kdl"):read("*a"))()
    load(io.popen("mycli usage | abs/path/cuc.exe generate [OPTIONS]"):read("*a"))()
    ```

5. For an example, you can check out [mise-clink](https://github.com/binyaminyblatt/mise-clink).

## Unsupported Features

There are some of the features currently unsupported by cuc generated completions, which may be supported by usage completions.

1. `config` and it's related properties.
2. `*_help`
3. `example`
4. `source_code_link_template`
5. `version`
6. `author`
7. `license`
8. `about`
9. `arg > parse, double_dash`
10. `flag > count, env, config, required_*, overrides`
11. `cmd > subcommand_required, mount`
12. `complete > descriptions`
