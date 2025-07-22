# cuc ([Clink](https://github.com/chrisant996/clink) [Usage](https://github.com/jdx/usage) Completions)

A CLI tool to generate clink argmatcher completions from the usage spec.

## Installation

1. Go to [Releases](https://github.com/IMXEren/cuc/releases/latest) and download the executable to a desired location.

2. The generated `usage.completions.lua` requires that you have these modules in your package.path (you can also use `!init.lua` or `.init.lua`, to ensure the modules are added to package.path):

    - [arghelper.lua](./modules/arghelper.lua)

3. For dynamic completion i.e. a usage.spec.kdl that uses `complete`, you'd need a shell while generating. The `complete` node in the spec uses run command that require unix shells. As a workaround, you can use git-bash which would work fine (CLI already uses it). So, you'd need to specify when using shell other than git-bash (or if not found) like MSYS2 environment.

4. For loading completions, you can either provide the spec from a file or by stdin.

```lua
load(io.popen("abs/path/cuc.exe generate [OPTIONS] abs/path/usage.kdl"):read("*a"))()
load(io.popen("mycli usage | abs/path/cuc.exe generate [OPTIONS]"):read("*a"))()
```
