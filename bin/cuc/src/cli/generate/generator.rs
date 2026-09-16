use std::{borrow::Borrow, collections::HashMap, path::PathBuf};

use cuc::namespace;

use super::formatter::GenFormatter;
use crate::{mbase64, string::StringExt};

#[derive(Default)]
pub struct Completor {
    pub exe_path: PathBuf,
    pub shell: PathBuf,
}

// Clink's built-in argmatcher generator is priority 24 (clink/lua/scripts/arguments.lua).
// Refresh mounted matchers immediately before it reads them.
const MOUNT_CONTEXT_GENERATOR_PRIORITY: u8 = 23;
const GENERATED_FUNCTION_TABLE: &str = "_cuc";

const MOUNT_CONTEXT: &str = r#"
local mount_context_updaters = {}
local mount_context_generator = clink.generator(__MOUNT_CONTEXT_GENERATOR_PRIORITY__)

function mount_context_generator:generate(line_state)
    for _, update in ipairs(mount_context_updaters) do
        update(line_state)
    end
    return false
end

local function cuc_shell_quote(word)
    return "'" .. word:gsub("'", "'\\''") .. "'"
end

-- Every helper below is local: a generated completion is self-contained, so two scripts
-- loaded into one Clink session cannot overwrite each other's helpers.
local function mount_path_reached(line_state, root_words, command_words, blocker_words, implicit_default)
    local command = line_state:getword(line_state:getcommandwordindex()):lower()
    local basename = path.getbasename(command):lower():gsub("%.exe$", "")
    if not root_words[command] and not root_words[basename] then return false end
    if not next(command_words) then return true end
    for index = line_state:getcommandwordindex() + 1, line_state:getwordcount() - 1 do
        local word = line_state:getword(index)
        if command_words[word] then return true end
        if implicit_default and blocker_words[word] then return false end
    end
    return implicit_default
end

local function mount_context_args(line_state, command_words, flags)
    if not next(flags) then return "" end

    local first = line_state:getcommandwordindex() + 1
    local last = line_state:getwordcount() - 1
    for index = first, last do
        if command_words[line_state:getword(index)] then
            last = index - 1
            break
        end
    end

    local args = {}
    local index = first
    while index <= last do
        local word = line_state:getword(index)
        local takes_arg = flags[word]
        if takes_arg ~= nil then
            table.insert(args, cuc_shell_quote(word))
            if takes_arg and index < last then
                index = index + 1
                table.insert(args, cuc_shell_quote(line_state:getword(index)))
            end
        else
            for name, has_arg in pairs(flags) do
                if has_arg and word:sub(1, #name + 1) == name .. "=" then
                    table.insert(args, cuc_shell_quote(word))
                    break
                end
            end
        end
        index = index + 1
    end
    return table.concat(args, " ")
end
"#;

const MOUNT_COLLECTOR: &str = r#"
local function mount_collector(loop_index)
    local collector = { args = {}, arg_index = 0, mode = "base", loop_index = loop_index }

    function collector:begin_base()
        self.mode = "base"
        self.arg_index = 0
    end

    function collector:begin_mount(replaces_flags, replaces_args)
        self.mode = "mount"
        self.replaces_flags = replaces_flags
        self.replaces_args = replaces_args
        self.arg_index = 0
        if replaces_flags then
            local globals = {}
            for _, flag in ipairs(self.flags or {}) do
                if type(flag) == "table" and type(flag[1]) == "table" then
                    table.insert(globals, flag)
                end
            end
            self.flags = globals
        end
        if replaces_args then
            self.saved_commands = {}
            for _, entry in ipairs(self.args[1] or {}) do
                if type(entry) == "table" and type(entry[1]) ~= "string" then
                    table.insert(self.saved_commands, entry)
                end
            end
            self.args = {}
            self.flags_anywhere = nil
            self.end_of_flags = nil
        end
    end

    function collector:_addexflags(flags)
        if self.flags == nil then self.flags = {} end
        for _, flag in ipairs(flags) do table.insert(self.flags, flag) end
        return self
    end

    local function merge_arg(target, source)
        for key, value in pairs(source) do
            if type(key) == "number" then
                table.insert(target, value)
            elseif key == "onlink" and target.onlink then
                local first = target.onlink
                target.onlink = function(...)
                    return first(...) or value(...)
                end
            else
                target[key] = value
            end
        end
    end

    function collector:_addexarg(arg)
        self.arg_index = self.arg_index + 1
        if self.mode == "base" or self.replaces_args then
            self.args[self.arg_index] = arg
            if self.arg_index == 1 and self.saved_commands then
                for _, command in ipairs(self.saved_commands) do table.insert(arg, command) end
                self.saved_commands = nil
            end
        elseif self.arg_index == 1 then
            if self.args[1] then merge_arg(self.args[1], arg) else self.args[1] = arg end
        end
        return self
    end

    function collector:setflagsanywhere(value)
        self.flags_anywhere = value
        return self
    end

    function collector:setendofflags(value)
        self.end_of_flags = value or true
        return self
    end

    function collector:nofiles()
        return self
    end

    function collector:apply(matcher)
        if self.flags then matcher:_addexflags(self.flags) end
        for _, arg in ipairs(self.args) do matcher:_addexarg(arg) end
        if self.flags_anywhere ~= nil then matcher:setflagsanywhere(self.flags_anywhere) end
        if self.end_of_flags ~= nil then
            if self.end_of_flags == true then
                matcher:setendofflags()
            else
                matcher:setendofflags(self.end_of_flags)
            end
        end
        if self.loop_index ~= nil then matcher:loop(self.loop_index) end
        matcher:nofiles()
    end

    return collector
end

"#;

/// Tokens that a positional must step over, plus the parser a clause separator links to.
#[derive(Default, Clone)]
struct ParserLinks {
    /// Tokens parsed by the position that follows, so the preceding positionals skip them.
    step_over: Vec<String>,
    /// A clause separator and the parser that continues the repeated clause group.
    clause: Option<(String, String)>,
    /// A usage `restart_token`: the token that begins another invocation of the command.
    restart_token: Option<String>,
    /// Parser entered after the first preserving positional consumes an explicit `--`.
    preserve: Option<String>,
}

#[derive(Default)]
pub struct Generator {
    pub spec: cuc::usage::UsageSpec,
    pub cached_functions: HashMap<String, String>,
    pub completor: Option<Completor>,
    pub arg_matchers: Vec<String>,
}

pub struct GeneratorView<'me> {
    pub spec: &'me cuc::usage::UsageSpec,
    pub cached_functions: &'me mut HashMap<String, String>,
    pub completor: Option<&'me Completor>,
    pub arg_matchers: &'me Vec<String>,
    /// In mount mode the chunk returns a function that adds this spec to an existing matcher.
    pub mount_prefix: Option<&'me str>,
    /// Outer parser boundary tokens that mounted positional arguments must yield to.
    pub mount_step_over: &'me [String],
}

struct MountBindOptions<'a> {
    flags: &'a [cuc::usage::Flag],
    command_words: &'a [String],
    blocker_words: &'a [String],
    implicit_default: bool,
    static_body: &'a str,
    step_over: &'a [String],
    loop_back: bool,
}

impl GeneratorView<'_> {
    pub fn generate(&mut self) -> String {
        let mut fmt = GenFormatter::default();
        if let Some(prefix) = self.mount_prefix {
            fmt.ns = cuc::namespace::NameSpace::root().join(namespace::slugify(prefix));
        }

        let mut script_start = r#"require("arghelper")
local base64 = require("base64")
local _cuc = {}

local function loop_until(word_index, line_state, user_data)
	if not user_data.first_index then
		user_data.first_index = word_index
	end
	local prev_word = line_state:getword(word_index - 1)
	-- "--" ends the argument, unless the spec preserves it as a value.
	if prev_word == "--" and not user_data.double_dash_preserve then
		return 1
	end
	-- Advance before the first word beyond the declared maximum.
	-- A negative maximum means the argument is unbounded.
	if user_data.var_max >= 0
		and word_index >= user_data.first_index + user_data.var_max
	then
		return 1
	end
	return 0
end

local function token_seen_before(token, word_index, line_state)
	for index = 1, word_index - 1 do
		if line_state:getword(index) == token then
			return true
		end
	end
	return false
end

local function double_dash_seen(word_index, line_state)
	return token_seen_before("--", word_index, line_state)
end

-- `word` is empty for the word under the cursor, and that word reports a zero length,
-- so read from its offset to the end of the line: a prefix check only needs its start.
local function sigil_word(word, word_index, line_state)
	if word ~= "" then
		return word
	end
	local info = line_state:getwordinfo(word_index)
	if not info then
		return ""
	end
	return line_state:getline():sub(info.offset)
end

"#
        .to_string();

        if Self::has_pending_mounts(self.spec) {
            script_start += &MOUNT_CONTEXT.replace(
                "__MOUNT_CONTEXT_GENERATOR_PRIORITY__",
                &MOUNT_CONTEXT_GENERATOR_PRIORITY.to_string(),
            );
            script_start += MOUNT_COLLECTOR;
        }

        if let Some(completor) = self.completor {
            script_start += &format!(
                r#"local function completor(word_index, line_state, b64_encoded_script, filter)
	local exec = [[{}]]
    local shell = [[{}]]
    local encoded_line = base64.encode(line_state:getline(), nil, true)
    local args = [[ complete --current ]] .. word_index - 1 .. [[ --line "]] .. encoded_line .. [[" --shell "]] .. shell .. [[" -- "]] .. b64_encoded_script .. [["]]
    local pipe, pclose, errcode = io.popen(exec .. args .. " 2>NUL")
    assert(pipe, "[ERROR]: failed to run complete command! err: " .. tostring(pclose) .. ", code: " .. tostring(errcode))
	-- Clink may return a pclose function as the second value when io.popen()
	-- is redirected to io.popenyield(). Otherwise, fall back to pipe:close().
	if type(pclose) ~= "function" then
		pclose = function()
			return pipe:close()
		end
	end
    local complete_args = {{}}
    for line in pipe:lines() do
        if filter then
            line = line:match("^([^:]+):") -- for filtering out descriptions
        end
        table.insert(complete_args, line)
    end
	local ok, _, code = pclose()
	if not ok then
		print("[ERROR]: failed to run complete command, exit code: " .. code)
	end
    return complete_args
end

"#,
                completor.exe_path.display(),
                completor.shell.display(),
            )
        }

        let mount_mode = self.mount_prefix.is_some();
        if mount_mode {
            fmt.level = 1;
        }
        let mut matcher_body = String::new();
        fmt.newline(&mut matcher_body);
        fmt.indent(&mut matcher_body);

        self.generate_cmd_functions(&self.spec.cmds, &mut fmt);
        let sigils = self.spec.sigils.clone();
        let body = self.add_flags(
            &self.spec.flags,
            &sigils,
            &self.spec.completes,
            self.spec.restart_token.as_deref(),
            &mut fmt,
        );
        if !body.is_empty() {
            matcher_body += &body;
            fmt.newline(&mut matcher_body);
            fmt.indent(&mut matcher_body);
        }

        let pending_mounts = self.spec.pending_mounts.clone();
        let mut all_args = Self::positional_args(&self.spec.args);
        Self::ensure_mount_arg(&mut all_args, &pending_mounts, self.completor.is_some());
        let root_func_name = Self::function_ref(fmt.ns.view().cmd_func_name("root"));
        let root_cmd = cuc::usage::Cmd {
            args: self.spec.args.clone(),
            flags: self.spec.flags.clone(),
            completes: self.spec.completes.clone(),
            sigils: self.spec.sigils.clone(),
            restart_token: self.spec.restart_token.clone(),
            clause: self.spec.clause.clone(),
            ..Default::default()
        };
        let parser_links = self.parser_links(&root_cmd, &root_func_name);
        let parser_links = self.generate_preserve_continuations(
            &root_func_name,
            &all_args,
            &self.spec.completes,
            &parser_links,
            &fmt,
        );
        matcher_body += &Self::add_preserve_marker(&parser_links);
        let args = self.generate_automatic_continuation(
            &root_func_name,
            &all_args,
            (&self.spec.flags, &self.spec.sigils),
            &self.spec.completes,
            &parser_links,
            &fmt,
        );
        if root_cmd
            .clause
            .as_ref()
            .is_some_and(|clause| clause.separator.is_some())
        {
            self.generate_clause_function(&root_cmd, &root_func_name, &self.spec.completes, &fmt);
        }
        let body = self.add_args_and_cmds(
            &self.spec.cmds,
            &args,
            &self.spec.completes,
            self.spec.default_subcommand.as_deref(),
            &parser_links,
            &mut fmt,
        );
        if !body.is_empty() {
            matcher_body += &body;
            fmt.newline(&mut matcher_body);
            fmt.indent(&mut matcher_body);
        }

        matcher_body += &Self::add_parser_policies(&args);
        let loop_back = Self::restart_loop(&parser_links);
        let mut script_body = if mount_mode {
            let loaders = self.mount_loader_body(
                &pending_mounts,
                &fmt.ns,
                "collector",
                &parser_links.step_over,
                "    ",
            );
            let replaces_flags = !self.spec.flags.is_empty();
            let replaces_args = !self.spec.args.is_empty() || self.spec.clause.is_some();
            let run_updaters = if Self::has_pending_mounts(self.spec) {
                "    if line_state then\n        for _, update in ipairs(mount_context_updaters) do update(line_state) end\n    end\n"
            } else {
                ""
            };
            format!(
                "\nreturn function(collector, line_state)\n    collector:begin_mount({replaces_flags}, {replaces_args})\n    collector{matcher_body}:nofiles()\n{loop_assign}{loaders}{run_updaters}end",
                loop_assign = if parser_links.restart_token.is_some() {
                    "    collector.loop_index = 1\n"
                } else {
                    ""
                },
            )
        } else {
            let matcher = if self.arg_matchers.is_empty() {
                format!("\nclink.argmatcher(\"{}\")", self.spec.info.bin)
            } else {
                "\nlocal matcher = clink.argmatcher()".to_string()
            };
            if let Some(init) = self.generate_mount_bind_function(
                &pending_mounts,
                &fmt.ns,
                MountBindOptions {
                    flags: &self.spec.flags,
                    command_words: &[],
                    blocker_words: &[],
                    implicit_default: false,
                    static_body: &matcher_body,
                    step_over: &parser_links.step_over,
                    loop_back: parser_links.restart_token.is_some(),
                },
            ) {
                format!("{init}({matcher}):nofiles()")
            } else {
                format!("{matcher}{matcher_body}{loop_back}:nofiles()")
            }
        };

        if !mount_mode && !self.arg_matchers.is_empty() {
            let mut arg_match_register_completion = String::new();
            fmt.newline(&mut arg_match_register_completion);
            for (i, arg_m) in self.arg_matchers.iter().enumerate() {
                if i != 0 {
                    fmt.newline(&mut arg_match_register_completion);
                }
                arg_match_register_completion +=
                    &format!("clink.arg.register_parser(\"{}\", matcher)", arg_m);
            }
            script_body += &arg_match_register_completion;
        }

        for func in self.cached_functions.values() {
            script_start += func;
        }

        script_start += &script_body;
        script_start
    }

    fn function_ref(name: impl AsRef<str>) -> String {
        format!("{GENERATED_FUNCTION_TABLE}.{}", name.as_ref())
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn lua_long_string(value: &str) -> String {
        let mut equals = String::new();
        while value.contains(&format!("]{equals}]")) {
            equals.push('=');
        }
        format!("[{equals}[{value}]{equals}]")
    }

    fn has_pending_mounts(spec: &cuc::usage::UsageSpec) -> bool {
        fn command_has_mounts(cmd: &cuc::usage::Cmd) -> bool {
            !cmd.pending_mounts.is_empty() || cmd.cmds.iter().any(|child| command_has_mounts(child))
        }
        !spec.pending_mounts.is_empty() || spec.cmds.iter().any(command_has_mounts)
    }

    fn shell_path(path: &std::path::Path) -> String {
        let path = path.display().to_string();
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .replace('\\', "/")
    }

    /// The mount command runs in the completion-time directory. A priority-23 generator
    /// updates only matchers whose command path is active before Clink's priority-24
    /// argmatcher generator reads them. This preserves inherited global flags without a
    /// general-purpose completion subprocess.
    fn mount_loader_body(
        &self,
        pending_mounts: &[cuc::usage::PendingMount],
        namespace: &cuc::namespace::NameSpace,
        target: &str,
        step_over: &[String],
        indent: &str,
    ) -> String {
        let Some(completor) = self.completor else {
            return String::new();
        };
        let arg_matchers = if self.arg_matchers.is_empty() {
            vec![self.spec.info.bin.as_str()]
        } else {
            self.arg_matchers.iter().map(String::as_str).collect()
        }
        .into_iter()
        .map(|name| format!(" --arg-matcher {}", Self::shell_quote(name)))
        .collect::<String>();
        let step_over_args = step_over
            .iter()
            .map(|token| format!(" --mount-step-over={}", Self::shell_quote(token)))
            .collect::<String>();
        let mut body = String::new();
        for (index, mount) in pending_mounts.iter().enumerate() {
            let prefix = if namespace.is_root() {
                format!("mount_{index}")
            } else {
                format!("{}_mount_{index}", namespace.display())
            };
            // Match Usage's mount behavior: shell-split the declaration and insert parsed
            // global flags immediately after the executable, not at the end of the command.
            let tokens =
                shell_words::split(&mount.run).expect("mount command should be valid shell syntax");
            let mount_command = if let Some((executable, arguments)) = tokens.split_first() {
                format!(
                    "{} __CUC_MOUNT_CONTEXT_ARGS__ {}",
                    shell_words::quote(executable),
                    shell_words::join(arguments)
                )
            } else {
                String::new()
            };
            let script = format!(
                "({mount_command}) | {} generate --mount {}{step_over_args} --complete --shell {}{}",
                Self::shell_quote(&Self::shell_path(&completor.exe_path)),
                Self::shell_quote(&prefix),
                Self::shell_quote(&Self::shell_path(&completor.shell)),
                arg_matchers,
            );
            let encoded = mbase64::encode(script);
            body += &format!(
                r#"{indent}do
{indent}    local script_path = os.tmpname()
{indent}    local script_file = assert(io.open(script_path, "wb"))
{indent}    local script_source = base64.decode([[{encoded}]], nil, true)
{indent}    script_source = script_source:gsub("__CUC_MOUNT_CONTEXT_ARGS__", function() return mount_args or "" end, 1)
{indent}    script_file:write(script_source)
{indent}    script_file:close()
{indent}    local shell = [[{shell}]]
{indent}    local shell_script_path = os.getfullpathname(script_path):gsub("\\", "/")
{indent}    local command = '""' .. shell .. '" "' .. shell_script_path .. '" 2>NUL"'
{indent}    local pipe, pclose, errcode = io.popen(command)
{indent}    assert(pipe, "[ERROR]: failed to resolve Usage mount! err: " .. tostring(pclose) .. ", code: " .. tostring(errcode))
{indent}    local chunk_source = pipe:read("*a")
{indent}    if type(pclose) ~= "function" then
{indent}        pclose = function() return pipe:close() end
{indent}    end
{indent}    local ok, _, code = pclose()
{indent}    os.remove(script_path)
{indent}    if ok then
{indent}        local chunk, load_error = load(chunk_source, "@cuc mount {prefix}")
{indent}        if chunk then
{indent}            local loaded, initialize = pcall(chunk)
{indent}            if loaded and type(initialize) == "function" then
{indent}                initialize({target}, line_state)
{indent}            else
{indent}                mount_ok = false
{indent}                print("[ERROR]: failed to load Usage mount: " .. tostring(initialize))
{indent}            end
{indent}        else
{indent}            mount_ok = false
{indent}            print("[ERROR]: failed to compile Usage mount: " .. tostring(load_error))
{indent}        end
{indent}    else
{indent}        mount_ok = false
{indent}        print("[ERROR]: failed to generate Usage mount, exit code: " .. tostring(code))
{indent}    end
{indent}end
"#,
                shell = completor.shell.display(),
                target = target,
            );
        }
        body
    }

    fn mount_flag_map(flags: &[cuc::usage::Flag]) -> String {
        let mut entries = Vec::new();
        for flag in flags.iter().filter(|flag| flag.is_global()) {
            let takes_arg = flag.arg.is_some();
            entries.extend(
                flag.names
                    .iter()
                    .map(|name| format!("[ {} ] = {takes_arg}", Self::lua_long_string(name))),
            );
            entries.extend(
                flag.aliases.iter().map(|alias| {
                    format!("[ {} ] = {takes_arg}", Self::lua_long_string(&alias.name))
                }),
            );
        }
        format!("{{{}}}", entries.join(", "))
    }

    fn generate_mount_bind_function(
        &mut self,
        pending_mounts: &[cuc::usage::PendingMount],
        namespace: &cuc::namespace::NameSpace,
        options: MountBindOptions<'_>,
    ) -> Option<String> {
        let MountBindOptions {
            flags: mount_flags,
            command_words: mount_command_words,
            blocker_words: mount_blocker_words,
            implicit_default,
            static_body,
            step_over,
            loop_back,
        } = options;
        if pending_mounts.is_empty() || self.completor.is_none() {
            return None;
        }
        let suffix = if namespace.is_root() {
            "root".to_string()
        } else {
            namespace::slugify(namespace.display())
        };
        let function_name = Self::function_ref(format!("_mount_bind_{suffix}"));
        let previous_context = format!("_mount_context_{suffix}");
        let cached_collector = format!("_mount_collector_{suffix}");
        let mount_root_words = std::iter::once(self.spec.info.bin.as_str())
            .chain(self.arg_matchers.iter().map(String::as_str))
            .flat_map(|word| {
                let lower = word.to_ascii_lowercase();
                let basename = lower
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&lower)
                    .trim_end_matches(".exe")
                    .to_string();
                [lower, basename]
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|word| format!("[ {} ] = true", Self::lua_long_string(&word)))
            .collect::<Vec<_>>()
            .join(", ");
        let mount_flags = Self::mount_flag_map(mount_flags);
        let mount_command_words = mount_command_words
            .iter()
            .map(|name| format!("[ {} ] = true", Self::lua_long_string(name)))
            .collect::<Vec<_>>()
            .join(", ");
        let mount_blocker_words = mount_blocker_words
            .iter()
            .map(|name| format!("[ {} ] = true", Self::lua_long_string(name)))
            .collect::<Vec<_>>()
            .join(", ");
        let loaders =
            self.mount_loader_body(pending_mounts, namespace, "collector", step_over, "    ");
        let function = format!(
            r#"local {previous_context}
local {cached_collector}
function {function_name}(matcher, track_context)
    if track_context ~= false then
    table.insert(mount_context_updaters, function(line_state)
        if not mount_path_reached(line_state, {{{mount_root_words}}}, {{{mount_command_words}}}, {{{mount_blocker_words}}}, {implicit_default}) then return end
        local mount_args = mount_context_args(line_state, {{{mount_command_words}}}, {mount_flags})
        local context = os.getcwd() .. "\0" .. mount_args
        if {previous_context} == context then return end
        local mount_ok = true
        matcher:reset()
        local collector = mount_collector({loop_index})
        collector:begin_base()
        collector{static_body}:nofiles()
{loaders}        collector:apply(matcher)
        if mount_ok then
            {previous_context} = context
            {cached_collector} = collector
        end
    end)
    end
    if {cached_collector} then {cached_collector}:apply(matcher) end
    return matcher
end
"#,
            loop_index = if loop_back { "1" } else { "nil" },
            implicit_default = if implicit_default { "true" } else { "false" },
        );
        self.cached_functions
            .insert(function_name.clone(), function);
        Some(function_name)
    }

    fn add_flags(
        &mut self,
        flags: &[cuc::usage::Flag],
        sigils: &[cuc::usage::Arg],
        completes: &HashMap<String, cuc::usage::Complete>,
        restart_token: Option<&str>,
        fmt: &mut GenFormatter,
    ) -> String {
        // Generate functions of returning anonymous clink.argmatcher
        // to link them to the corresponding flag
        self.generate_flag_functions(flags, completes, fmt);
        // A sigil is matched anywhere flags are, so its literal candidates belong in the
        // flag list rather than in a positional slot.
        let sigil_bodies = self.sigil_flag_bodies(sigils, restart_token);
        let mut completions = String::new();

        let mut found_global_flag = false;
        let mut found_non_global_flag = false;

        let entry_start = |mut completions: &mut String, fmt: &mut GenFormatter| {
            *completions += r#":_addexflags({"#;
            fmt.increment_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
        };

        let entry_delim = |fmt: &GenFormatter| {
            let mut completions = String::new();
            completions += ",";
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions
        };

        let entry_close = |mut completions: &mut String, fmt: &mut GenFormatter| {
            fmt.decrement_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            *completions += "})";
        };

        if flags.is_empty() && !sigil_bodies.is_empty() {
            entry_start(&mut completions, fmt);
        }

        for (index, flag) in flags.iter().enumerate() {
            if index == 0 {
                entry_start(&mut completions, fmt);
            }

            // Add all non global flags
            if !flag.is_global() {
                let body = self.add_flag_body(flag, fmt);
                if !body.is_empty() {
                    completions += &body;
                    completions += &entry_delim(fmt);
                }
                found_non_global_flag = true;
            } else {
                found_global_flag = true;
            }
        }

        for body in sigil_bodies.iter() {
            completions += body;
            completions += &entry_delim(fmt);
            found_non_global_flag = true;
        }

        let ns = fmt.ns.view();
        if !flags.is_empty() || found_non_global_flag {
            let entry_delim = entry_delim(fmt);

            // A namespace only defines a global flag function when it declares global
            // flags itself, so an inherited global may belong to any ancestor. Walk the
            // whole lineage and only link the ones that were actually generated.
            let global_funcs = if found_global_flag {
                ns.lineage()
                    .into_iter()
                    .map(|ancestor| Self::function_ref(ancestor.global_flag_func_name()))
                    .filter(|func_name| {
                        self.cached_functions
                            .get(func_name)
                            .is_some_and(|function| !function.is_empty())
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            if global_funcs.is_empty() {
                // Trimming the entry_delim as to safely close
                completions.trim_end_matches_mut(&entry_delim);
            } else {
                if found_non_global_flag && !completions.ends_with(&entry_delim) {
                    completions += &entry_delim;
                }
                for (index, func_name) in global_funcs.iter().enumerate() {
                    if index != 0 {
                        completions += &entry_delim;
                    }
                    completions += func_name;
                    completions += "()";
                }
            }

            entry_close(&mut completions, fmt);
        }
        completions
    }

    /// A sigil argument is classified by a leading prefix instead of by its place in
    /// the positional sequence, so it is matched anywhere a flag is and every candidate
    /// carries the prefix. Only fixed candidates can be delivered this way; a sigil with
    /// a runtime completer is completed at its own argument position instead.
    fn sigil_flag_bodies(
        &self,
        sigils: &[cuc::usage::Arg],
        restart_token: Option<&str>,
    ) -> Vec<String> {
        let mut bodies = Vec::new();
        for arg in sigils.iter().filter(|arg| arg.sigil.is_some()) {
            if arg.choices.is_empty() {
                continue;
            }
            let sigil = arg.sigil.clone().unwrap_or_default();
            let mut entries = Vec::new();
            let mut hidden_entries = Vec::new();
            let mut generated_matches = Vec::new();
            for choice in arg.choices.iter() {
                // One entry per candidate: a table's later values are a description, not
                // more alternatives.
                let candidate = format!("{sigil}{choice}");
                let mut entry = format!("{{ \"{candidate}\"");
                let mut generated = format!("{{ match = \"{candidate}\"");
                if !arg.repr.is_empty() {
                    let description = arg.repr.replace('"', "\\\"");
                    entry += &format!(", \" {description}\"");
                    generated += &format!(", description = \" {description}\"");
                }
                entry += " }";
                generated += " }";
                entries.push(entry);
                hidden_entries.push(format!("{{ \"{candidate}\", hide=true }}"));
                generated_matches.push(generated);
            }
            if let Some(token) = restart_token {
                bodies.extend(hidden_entries);
                bodies.push(format!(
                    "function(_, word_index, line_state) if token_seen_before(\"{token}\", word_index, line_state) then return {{}} end return {{{}}} end",
                    generated_matches.join(", ")
                ));
            } else {
                bodies.extend(entries);
            }
        }
        bodies
    }

    fn sigil_func_name(arg: &cuc::usage::Arg, complete: &cuc::usage::Complete) -> String {
        Self::function_ref(namespace::sigil_arg_func_name(format!(
            "{}::{}::{}",
            complete.name,
            arg.name,
            arg.sigil.as_deref().unwrap_or_default()
        )))
    }

    fn add_flag_body(&self, flag: &cuc::usage::Flag, fmt: &GenFormatter) -> String {
        let ns = fmt.ns.view();
        let mut completions = String::new();
        let flag_name = namespace::slugify(&flag.name);
        let func_name = Self::function_ref(ns.flag_func_name(&flag_name));
        let mut flag_names = flag.names.clone();
        flag.aliases
            .iter()
            .filter(|alias| !alias.hide)
            .for_each(|alias| flag_names.push(alias.name.clone()));
        for (inner_index, name) in flag_names.iter().enumerate() {
            if inner_index != 0 {
                completions += ",";
                fmt.newline(&mut completions);
                fmt.indent(&mut completions);
                completions += "--[[alias]] ";
            }

            completions += "{ \"";
            completions += name;
            completions += "\"";

            if self
                .cached_functions
                .get(&func_name)
                .is_some_and(|function| !function.is_empty())
            {
                completions += " .. ";
                completions += &func_name;
                completions += "() ";
            }
            if flag.arg.is_none()
                && let Some(command) = &flag.link_to
            {
                let command_func =
                    Self::function_ref(fmt.ns.view().cmd_func_name(namespace::slugify(command)));
                if self
                    .cached_functions
                    .get(&command_func)
                    .is_some_and(|function| !function.is_empty())
                {
                    completions += " .. ";
                    completions += &command_func;
                    completions += "() ";
                }
            }

            if let Some(ref arg) = flag.arg {
                completions += ", ";
                completions += "\" ";
                completions += &arg.repr;
                completions += "\"";
            }

            if !flag.help.is_empty() {
                completions += ", ";
                completions += "[===[";
                completions += &flag.help;
                completions += "]===]";
            }
            completions += " }";

            /*
            * Format:

            { "FLAG" .. _flag_FUNC_NAME(), [" ARG_INFO"], [" FLAG_HELP"] }
            --[[alias]] { "FLAG" .. _flag_FUNC_NAME(), [" ARG_INFO"], [" FLAG_HELP"] }
            */
        }

        completions
    }

    fn add_arg_start() -> String {
        ":_addexarg({".into()
    }

    fn add_arg_hint(arg: &cuc::usage::Arg) -> String {
        if arg.hide {
            return "hint = nil".to_string();
        }
        let mut completions = String::from("hint = [===[Argument expected: ");
        completions += &arg.repr;
        if let Some(sigil) = &arg.sigil {
            completions += &format!(" [prefix: {sigil}]");
        }
        if arg.var {
            let var_min = arg.min.unwrap();
            let var_max = arg
                .max
                .map(|i| if i < 0 { "*".into() } else { i.to_string() })
                .unwrap();
            completions += &format!(" [multiple args ({}..{})]", var_min, var_max);
        }
        if let Some(ref default) = arg.default
            && !default.is_empty()
        {
            completions += &format!(" [default: {}]", default);
        }
        completions += "]===]";
        completions
    }

    fn add_arg_loop_until(arg: &cuc::usage::Arg, links: &ParserLinks) -> String {
        let mut body = Vec::new();
        if arg.skip_after_double_dash {
            body.push("if double_dash_seen(wi,ls) then return 1 end".to_string());
        }
        // A sigil argument never takes the ordinary positional cursor: it is classified by
        // its prefix, so a word without the prefix steps over this position.
        if let Some(sigil) = &arg.sigil {
            body.push(format!(
                "if sigil_word(word, wi, ls):sub(1, {}) ~= \"{sigil}\" then return 1 end",
                sigil.len()
            ));
        }
        // A restart token or clause separator is parsed by the position that follows, so
        // every preceding positional must step over it instead of taking it as a value.
        if !links.step_over.is_empty() {
            let conditions = links
                .step_over
                .iter()
                .map(|token| {
                    if links
                        .clause
                        .as_ref()
                        .is_some_and(|(separator, _)| separator == token)
                    {
                        format!("(word == \"{token}\" and not double_dash_seen(wi,ls))")
                    } else {
                        format!("word == \"{token}\"")
                    }
                })
                .collect::<Vec<_>>()
                .join(" or ");
            body.push(format!("if {conditions} then return 1 end"));
        }
        if arg.double_dash == cuc::usage::DoubleDash::Preserve {
            body.push("ud.double_dash_preserve=true".to_string());
        }
        if arg.double_dash == cuc::usage::DoubleDash::Required {
            body.push(
                "if not ud.double_dash_started then if not double_dash_seen(wi,ls) then return 1 end; ud.double_dash_started=true; ud.double_dash_preserve=true end"
                    .to_string(),
            );
        }
        if arg.var {
            body.push(format!(
                "ud.var_min={}; ud.var_max={}; return loop_until(wi,ls,ud)",
                arg.min.unwrap(),
                arg.max.unwrap(),
            ));
        }
        if body.is_empty() {
            String::new()
        } else {
            format!(
                ", onadvance = function(_,word,wi,ls,ud) {} end",
                body.join("; ")
            )
        }
    }

    fn add_arg_link(arg: &cuc::usage::Arg, links: &ParserLinks) -> String {
        let mut conditions = links
            .clause
            .iter()
            .map(|(token, func_name)| {
                format!("if word == \"{token}\" then return {func_name}() end")
            })
            .collect::<Vec<_>>();
        if let Some(link) = &arg.link_after {
            if arg.var {
                if let Some(max) = arg.max.filter(|max| *max >= 0) {
                    conditions.push(format!(
                        "if ud.first_index and wi >= ud.first_index + {max} - 1 then return {link}() end"
                    ));
                }
            } else {
                conditions.push(format!("return {link}()"));
            }
        }
        if conditions.is_empty() {
            String::new()
        } else {
            format!(
                ", onlink = function(_,_,word,wi,_,ud) {} end",
                conditions.join("; ")
            )
        }
    }

    fn add_arg_close(arg: Option<&cuc::usage::Arg>, links: &ParserLinks) -> String {
        let mut completions = String::new();
        if let Some(arg) = arg {
            completions += &Self::add_arg_hint(arg);
            completions += &Self::add_arg_loop_until(arg, links);
            completions += &Self::add_arg_link(arg, links);
        }
        completions += "})";
        completions
    }

    /// Clink defaults to recognizing flags anywhere. `double_dash=automatic` means
    /// flags stop being recognized once a positional value has been entered, which
    /// is exactly what `setflagsanywhere(false)` does. All non-preserving modes use
    /// Clink's end-of-flags marker so an explicit `--` also disables later flags.
    fn add_parser_policies(args: &[cuc::usage::Arg]) -> String {
        let mut policies = String::new();
        if args
            .iter()
            .any(|arg| arg.double_dash == cuc::usage::DoubleDash::Automatic)
        {
            policies += ":setflagsanywhere(false)";
        }
        if !args.is_empty()
            && !args
                .iter()
                .any(|arg| arg.double_dash == cuc::usage::DoubleDash::Preserve)
        {
            policies += ":setendofflags()";
        }
        policies
    }

    /// A `restart_token` sits in the last argument position, so `loop(1)` makes the token
    /// start another invocation. The repetition is unbounded and needs no parser links.
    fn restart_loop(links: &ParserLinks) -> &'static str {
        if links.restart_token.is_some() {
            ":loop(1)"
        } else {
            ""
        }
    }

    fn enclose_arg(matcher: Option<&str>, arg: &cuc::usage::Arg, links: &ParserLinks) -> String {
        let mut completion = Self::add_arg_start();
        if let Some(matcher) = matcher.filter(|matcher| !matcher.is_empty()) {
            completion += matcher;
            completion += ", "; // Adding ',' because required by hint
        }
        completion += &Self::add_arg_close(Some(arg), links);
        completion
    }

    /// @param enclose: add start and close to string
    fn add_arg(
        &mut self,
        arg: &cuc::usage::Arg,
        completes: &HashMap<String, cuc::usage::Complete>,
        enclose: bool,
        links: &ParserLinks,
    ) -> String {
        let matcher = if arg.hide {
            Some("function() return {} end".to_string())
        } else if arg.sigil.is_some() && arg.choices.is_empty() {
            // A dynamic sigil needs a wrapper that restores the prefix. Even when
            // --complete is absent, its empty position remains so onadvance can skip it.
            self.find_arg_complete(arg, completes)
                .cloned()
                .filter(|complete| match complete.kind {
                    cuc::usage::CompleteKind::None => false,
                    cuc::usage::CompleteKind::Run(_) => self.completor.is_some(),
                    cuc::usage::CompleteKind::File | cuc::usage::CompleteKind::Dir => true,
                })
                .map(|complete| {
                    self.generate_sigil_arg_function(
                        arg,
                        &complete,
                        links.restart_token.as_deref(),
                    );
                    Self::sigil_func_name(arg, &complete)
                })
        } else if !arg.choices.is_empty() {
            Some(format!("\"{}\"", arg.choices.join(r#"", ""#)))
        } else {
            // Just add input hints for helping when no matcher is available.
            self.find_arg_complete(arg, completes)
                .cloned()
                .and_then(|complete| match complete.kind {
                    cuc::usage::CompleteKind::File => Some("clink.filematches".to_string()),
                    cuc::usage::CompleteKind::Dir => Some("clink.dirmatches".to_string()),
                    cuc::usage::CompleteKind::Run(_) if self.completor.is_some() => {
                        self.generate_arg_complete_function(&complete);
                        Some(Self::function_ref(namespace::arg_complete_func_name(
                            &complete.name,
                        )))
                    }
                    _ => None,
                })
        };

        if enclose {
            Self::enclose_arg(matcher.as_deref(), arg, links)
        } else {
            matcher.unwrap_or_default()
        }
    }

    fn add_args_and_cmds<C, A>(
        &mut self,
        cmds: &[C],
        args: &[A],
        completes: &HashMap<String, cuc::usage::Complete>,
        default_subcommand: Option<&str>,
        links: &ParserLinks,
        fmt: &mut GenFormatter,
    ) -> String
    where
        C: Borrow<cuc::usage::Cmd>,
        A: Borrow<cuc::usage::Arg>,
    {
        // Generate functions to be linked with subcmds
        self.generate_cmd_functions(cmds, fmt);
        let mut completions = String::new();
        let mut arg: Option<&cuc::usage::Arg> = None;
        let mut started = false;
        let mut has_entry = false;

        let entry_start = |completions: &mut String, fmt: &mut GenFormatter| {
            fmt.increment_level();
            *completions += &Self::add_arg_start();
            fmt.newline(completions);
            fmt.indent(completions);
        };

        let entry_delim = |fmt: &GenFormatter| {
            let mut completions = String::new();
            completions += ",";
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions
        };

        let entry_close = |completions: &mut String, fmt: &mut GenFormatter| {
            fmt.decrement_level();
            fmt.newline(completions);
            fmt.indent(completions);
            *completions += &Self::add_arg_close(None, &ParserLinks::default());
        };

        if !args.is_empty() {
            arg = Some(args[0].borrow());
            let arg = arg.unwrap();
            let arg_completion = self.add_arg(arg, completes, false, links);
            entry_start(&mut completions, fmt);
            started = true;
            if !arg_completion.is_empty() {
                completions += &arg_completion;
                has_entry = true;
            }
        }

        for cmd in cmds.iter() {
            let cmd: &cuc::usage::Cmd = cmd.borrow();
            let cmd_name = namespace::slugify(&cmd.name);
            let func_name = Self::function_ref(fmt.ns.view().cmd_func_name(&cmd_name));

            if !started {
                entry_start(&mut completions, fmt);
                started = true;
            } else if has_entry {
                completions += &entry_delim(fmt);
            }

            let mut cmd_names = vec![&cmd.name];
            cmd.aliases
                .iter()
                .filter(|alias| !alias.hide)
                .for_each(|alias| cmd_names.push(&alias.name));

            for (inner_index, name) in cmd_names.into_iter().enumerate() {
                if inner_index != 0 {
                    completions += ",";
                    fmt.newline(&mut completions);
                    fmt.indent(&mut completions);
                    completions += "--[[alias]] ";
                }

                completions += "{ \"";
                completions += name;
                completions += "\"";

                if self
                    .cached_functions
                    .get(&func_name)
                    .is_some_and(|function| !function.is_empty())
                {
                    completions += " .. ";
                    completions += &func_name;
                    completions += "()";
                }
                if !cmd.help.is_empty() {
                    completions += &format!(", [===[{}]===]", cmd.help);
                }
                completions += " }";
                has_entry = true;
            }
        }

        if let Some(default_subcommand) = default_subcommand {
            // Clink has no implicit-subcommand primitive. onlink is the closest mapping:
            // known subcommands still link normally and other first words use the default.
            let command_name = namespace::slugify(default_subcommand);
            let function_name = Self::function_ref(fmt.ns.view().cmd_func_name(&command_name));
            if self
                .cached_functions
                .get(&function_name)
                .is_some_and(|function| !function.is_empty())
            {
                if !started {
                    entry_start(&mut completions, fmt);
                    started = true;
                } else if has_entry {
                    completions += &entry_delim(fmt);
                }
                completions +=
                    &format!("onlink = function(link) return link or {function_name}() end");
                has_entry = true;
            }
        }

        if started {
            if let Some(arg) = arg {
                if has_entry {
                    completions += &entry_delim(fmt);
                }
                completions += &Self::add_arg_hint(arg);
                completions += &Self::add_arg_loop_until(arg, links);
                completions += &Self::add_arg_link(arg, links);
            }

            entry_close(&mut completions, fmt);
        }

        if args.len() > 1 {
            for arg in &args[1..] {
                let arg = arg.borrow();
                fmt.newline(&mut completions);
                fmt.indent(&mut completions);
                completions += &self.add_arg(arg, completes, true, links);
            }
        }

        // A clause separator is consumed by a position of its own that links to the parser
        // repeating the clause group.
        if let Some((token, func_name)) = &links.clause {
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &Self::add_arg_start();
            fmt.increment_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &format!(
                "function(_,wi,ls) if not double_dash_seen(wi,ls) then return {{\"{token}\"}} end return {{}} end"
            );
            completions += &entry_delim(fmt);
            completions += &format!(
                "onlink = function(_,_,word,wi,ls) if word == \"{token}\" and not double_dash_seen(wi,ls) then return {func_name}() end end"
            );
            fmt.decrement_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += "})";
        }

        // The restart token is the last position, so `loop(1)` restarts the invocation.
        if let Some(token) = &links.restart_token {
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &Self::add_arg_start();
            fmt.increment_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &format!("\"{token}\"");
            fmt.decrement_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += "})";
        }

        completions
    }

    fn generate_flag_functions(
        &mut self,
        flags: &[cuc::usage::Flag],
        completes: &HashMap<String, cuc::usage::Complete>,
        fmt: &GenFormatter,
    ) {
        let ns = fmt.ns.view();
        let mut global_flags: Vec<&cuc::usage::Flag> = vec![];
        for flag in flags.iter() {
            if flag.is_global() {
                /*
                    In UsageSpecExt, we recursively add global flags (if not already present) to their subsequent cmds.
                    But cmds can have their own flags marked as global. So, now there are two possibilities, that the global flag
                    added to cmd is either imposed (set by parent) or set by itself (should be imposed on others).

                    Don't add child flags of subcmds marked as global (i.e. imposed by parent).
                    For example, let's say --cd (root; _flag_cd) is marked as global and root subcmds are 'foo', 'bar'
                    So, the usagespec loader would add --cd flag to foo and bar marking them as globals (_flag_foo_cd, _flag_bar_cd)
                    Now, we need to skip (_flag_foo_cd and _flag_bar_cd) because they are copies of _flag_cd. We can check that
                    _global_flags_PARENT (ns.parent().global_flag_func_name()) and _flag_FLAG (ns.parent().flag_func_name(&flag.name))
                    exist to confirm if it was imposed.

                    Solution: I added GlobalFlag::Imposed(NameSpace) for now to simplify it.
                */

                if flag.is_global_imposed() {
                    continue;
                } else {
                    global_flags.push(flag);
                }
            }

            let flag_name = namespace::slugify(&flag.name);
            let func_name = Self::function_ref(ns.flag_func_name(&flag_name));
            let mut function = String::new();
            if let Some(ref arg) = flag.arg {
                let mut arg = arg.clone();
                if let Some(command) = &flag.link_to {
                    arg.link_after = Some(Self::function_ref(
                        ns.cmd_func_name(namespace::slugify(command)),
                    ));
                }
                let arg_completion = self.add_arg(&arg, completes, true, &ParserLinks::default());
                if !arg_completion.is_empty() {
                    function += "function ";
                    function += &func_name;
                    function += r#"()
    return clink.argmatcher()"#;
                    function += &arg_completion;
                    if !function.ends_with("\n") {
                        function += "\n";
                    }
                    function += "end\n";
                }
            }
            self.cached_functions.insert(func_name, function);
        }

        if !global_flags.is_empty() {
            let mut body = String::new();
            for (index, gflag) in global_flags.into_iter().enumerate() {
                if index != 0 {
                    body += ",";
                    fmt.newline(&mut body);
                    fmt.indent(&mut body);
                    fmt.indent(&mut body);
                }
                body += &self.add_flag_body(gflag, fmt);
            }

            let func_name = Self::function_ref(ns.global_flag_func_name());
            let mut function = String::new();
            if !body.is_empty() {
                function += "function ";
                function += &func_name;
                function += r#"()
    return {
        "#;
                function += &body;
                function += r#"
    }"#;
                if !function.ends_with("\n") {
                    function += "\n";
                }
                function += "end\n";
            }
            self.cached_functions.insert(func_name, function);
        }
    }

    fn generate_cmd_functions<C>(&mut self, cmds: &[C], fmt: &mut GenFormatter)
    where
        C: Borrow<cuc::usage::Cmd>,
    {
        let mut chfmt = fmt.clone();
        for cmd in cmds.iter() {
            let cmd: &cuc::usage::Cmd = cmd.borrow();
            let cmd_name = namespace::slugify(&cmd.name);
            chfmt.ns = fmt.ns.clone().join(&cmd_name);

            let mut function = String::new();
            let func_name = Self::function_ref(fmt.ns.view().cmd_func_name(&cmd_name));

            let subcmds = cmd.cmds.as_slice();
            let mut all_args = Self::positional_args(&cmd.args);
            Self::ensure_mount_arg(&mut all_args, &cmd.pending_mounts, self.completor.is_some());
            // A mounted command needs a parser link even when its own matcher is empty.
            // Crossing that boundary stops flags from the mounting command from being offered
            // afterward; Clink still returns to the parent's restart loop when appropriate.
            if self.mount_prefix.is_some()
                || !cmd.flags.is_empty()
                || !cmd.sigils.is_empty()
                || !subcmds.is_empty()
                || !all_args.is_empty()
                || (!cmd.pending_mounts.is_empty() && self.completor.is_some())
            {
                let mut cmd_completion = String::new();

                let completion = self.add_flags(
                    &cmd.flags,
                    &cmd.sigils,
                    &cmd.completes,
                    cmd.restart_token.as_deref(),
                    &mut chfmt,
                );
                if !completion.is_empty() {
                    fmt.newline(&mut cmd_completion);
                    fmt.indent(&mut cmd_completion);
                    cmd_completion += &completion;
                }

                let parser_links = self.parser_links(cmd, &func_name);
                let parser_links = self.generate_preserve_continuations(
                    &func_name,
                    &all_args,
                    &cmd.completes,
                    &parser_links,
                    &chfmt,
                );
                cmd_completion += &Self::add_preserve_marker(&parser_links);
                let args = self.generate_automatic_continuation(
                    &func_name,
                    &all_args,
                    (&cmd.flags, &cmd.sigils),
                    &cmd.completes,
                    &parser_links,
                    &chfmt,
                );
                let completion = self.add_args_and_cmds(
                    subcmds,
                    &args,
                    &cmd.completes,
                    None,
                    &parser_links,
                    &mut chfmt,
                );
                if !completion.is_empty() {
                    fmt.newline(&mut cmd_completion);
                    fmt.indent(&mut cmd_completion);
                    cmd_completion += &completion;
                }

                let policies = Self::add_parser_policies(&args);
                let loop_back = Self::restart_loop(&parser_links);
                let mount_command_words = std::iter::once(cmd.name.clone())
                    .chain(cmd.aliases.iter().map(|alias| alias.name.clone()))
                    .collect::<Vec<_>>();
                let implicit_default = fmt.ns.is_root()
                    && self.spec.default_subcommand.as_deref() == Some(cmd.name.as_str());
                let mount_blocker_words = if implicit_default {
                    cmds.iter()
                        .map(Borrow::borrow)
                        .filter(|sibling: &&cuc::usage::Cmd| sibling.name != cmd.name)
                        .flat_map(|sibling| {
                            std::iter::once(sibling.name.clone())
                                .chain(sibling.aliases.iter().map(|alias| alias.name.clone()))
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let mount_bind = self.generate_mount_bind_function(
                    &cmd.pending_mounts,
                    &chfmt.ns,
                    MountBindOptions {
                        flags: &cmd.mount_prefix_flags,
                        command_words: &mount_command_words,
                        blocker_words: &mount_blocker_words,
                        implicit_default,
                        static_body: &format!("{cmd_completion}{policies}"),
                        step_over: &parser_links.step_over,
                        loop_back: parser_links.restart_token.is_some(),
                    },
                );
                if let Some(init) = &mount_bind {
                    function += &format!(
                        "function {func_name}()\n    return {init}(clink.argmatcher())\nend\n"
                    );
                } else {
                    function += &format!("function {func_name}()\n    return clink.argmatcher()");
                    function += &cmd_completion;
                    function += &policies;
                    function += loop_back;
                    if !function.ends_with('\n') {
                        fmt.newline(&mut function);
                    }
                    function += "end\n";
                }
                if cmd
                    .clause
                    .as_ref()
                    .is_some_and(|clause| clause.separator.is_some())
                {
                    self.generate_clause_function(cmd, &func_name, &cmd.completes, &chfmt);
                }
            }

            self.cached_functions.insert(func_name, function);
        }
    }

    fn parser_links(&self, cmd: &cuc::usage::Cmd, func_name: &str) -> ParserLinks {
        let mut links = ParserLinks::default();
        if let Some(token) = &cmd.restart_token {
            links.step_over.push(token.clone());
            links.restart_token = Some(token.clone());
        }
        if let Some(clause) = &cmd.clause
            && let Some(separator) = &clause.separator
        {
            links.step_over.push(separator.clone());
            links.clause = Some((separator.clone(), format!("{func_name}_clause")));
        }
        self.inherit_mount_step_over(&mut links);
        links
    }

    fn inherit_mount_step_over(&self, links: &mut ParserLinks) {
        for token in self.mount_step_over {
            if !links.step_over.contains(token) {
                links.step_over.push(token.clone());
            }
        }
    }

    fn add_preserve_marker(links: &ParserLinks) -> String {
        links
            .preserve
            .as_ref()
            .map(|continuation| format!(":_addexflags({{{{ \"--\" .. {continuation}() }}}})"))
            .unwrap_or_default()
    }

    /// Build the flagless continuation entered after the first preserving positional
    /// consumes `--`. Registering the marker as a linked flag makes Clink disable later
    /// flags without losing the marker's positional step.
    fn generate_preserve_continuations(
        &mut self,
        func_name: &str,
        args: &[cuc::usage::Arg],
        completes: &HashMap<String, cuc::usage::Complete>,
        links: &ParserLinks,
        fmt: &GenFormatter,
    ) -> ParserLinks {
        let mut result = links.clone();
        let Some((index, arg)) = args
            .iter()
            .enumerate()
            .find(|(_, arg)| arg.double_dash == cuc::usage::DoubleDash::Preserve)
        else {
            return result;
        };

        let continuation = format!("{func_name}_preserve_{index}");
        let start = if arg.var { index } else { index + 1 };
        let continuation_args = args[start..]
            .iter()
            .filter(|arg| arg.sigil.is_none())
            .cloned()
            .collect::<Vec<_>>();
        let mut continuation_links = links.clone();
        continuation_links.preserve = None;
        if let Some((separator, _)) = &continuation_links.clause {
            continuation_links
                .step_over
                .retain(|token| token != separator);
        }
        continuation_links.clause = None;

        let no_cmds: [cuc::usage::Cmd; 0] = [];
        let mut chfmt = fmt.clone();
        let body = self.add_args_and_cmds(
            &no_cmds,
            &continuation_args,
            completes,
            None,
            &continuation_links,
            &mut chfmt,
        );
        let mut function = format!("function {continuation}()\n    return clink.argmatcher()");
        function += &body;
        function += Self::restart_loop(&continuation_links);
        if body.is_empty() && continuation_links.restart_token.is_none() {
            function += ":nofiles()";
        }
        if !function.ends_with('\n') {
            chfmt.newline(&mut function);
        }
        function += "end\n";
        self.cached_functions.insert(continuation.clone(), function);
        result.preserve = Some(continuation);
        result
    }

    /// Split at a later `double_dash=automatic` argument. The prefix parser keeps flags
    /// available; after its last positional is consumed, onlink enters a parser whose
    /// `setflagsanywhere(false)` starts exactly at the automatic argument.
    fn generate_automatic_continuation(
        &mut self,
        func_name: &str,
        args: &[cuc::usage::Arg],
        flag_inputs: (&[cuc::usage::Flag], &[cuc::usage::Arg]),
        completes: &HashMap<String, cuc::usage::Complete>,
        links: &ParserLinks,
        fmt: &GenFormatter,
    ) -> Vec<cuc::usage::Arg> {
        let Some(index) = args
            .iter()
            .position(|arg| arg.double_dash == cuc::usage::DoubleDash::Automatic)
        else {
            return args.to_vec();
        };
        if index == 0 {
            return args.to_vec();
        }

        let (flags, sigils) = flag_inputs;
        let continuation = format!("{func_name}_automatic");
        let mut chfmt = fmt.clone();
        let mut body = String::new();
        let flag_body = self.add_flags(
            flags,
            sigils,
            completes,
            links.restart_token.as_deref(),
            &mut chfmt,
        );
        if !flag_body.is_empty() {
            chfmt.newline(&mut body);
            chfmt.indent(&mut body);
            body += &flag_body;
        }
        let no_cmds: [cuc::usage::Cmd; 0] = [];
        if args[index..]
            .iter()
            .any(|arg| arg.double_dash == cuc::usage::DoubleDash::Preserve)
        {
            body += &Self::add_preserve_marker(links);
        }
        let args_body =
            self.add_args_and_cmds(&no_cmds, &args[index..], completes, None, links, &mut chfmt);
        if !args_body.is_empty() {
            chfmt.newline(&mut body);
            chfmt.indent(&mut body);
            body += &args_body;
        }
        let mut function = format!("function {continuation}()\n    return clink.argmatcher()");
        function += &body;
        function += &Self::add_parser_policies(&args[index..]);
        function += Self::restart_loop(links);
        if !function.ends_with('\n') {
            chfmt.newline(&mut function);
        }
        function += "end\n";
        self.cached_functions.insert(continuation.clone(), function);

        let mut prefix = args[..index].to_vec();
        prefix
            .last_mut()
            .expect("automatic argument has a prefix")
            .link_after = Some(continuation);
        prefix
    }

    /// Sigil arguments with fixed candidates are matched anywhere through the flag list, so
    /// they must not also take a positional slot.
    fn positional_args(args: &[cuc::usage::Arg]) -> Vec<cuc::usage::Arg> {
        args.iter()
            .filter(|arg| arg.sigil.is_none() || arg.choices.is_empty())
            .cloned()
            .collect()
    }

    fn ensure_mount_arg(
        args: &mut Vec<cuc::usage::Arg>,
        pending_mounts: &[cuc::usage::PendingMount],
        enabled: bool,
    ) {
        if !enabled || pending_mounts.is_empty() || !args.is_empty() {
            return;
        }
        let synopsis = pending_mounts
            .iter()
            .find_map(|mount| mount.synopsis.clone())
            .unwrap_or_else(|| "[MOUNT]".to_string());
        args.push(cuc::usage::Arg {
            name: synopsis.clone(),
            repr: synopsis,
            var: true,
            min: Some(0),
            max: Some(-1),
            ..Default::default()
        });
    }

    /// Build one repeated clause group. The separator links lazily to another parser built
    /// by this function, so an explicit clause can repeat without a fixed depth limit.
    fn generate_clause_function(
        &mut self,
        cmd: &cuc::usage::Cmd,
        func_name: &str,
        completes: &HashMap<String, cuc::usage::Complete>,
        fmt: &GenFormatter,
    ) {
        let clause = cmd.clause.as_ref().expect("clause function without clause");
        let separator = clause
            .separator
            .as_ref()
            .expect("clause function without separator");
        let clause_func_name = format!("{func_name}_clause");
        let no_cmds: [cuc::usage::Cmd; 0] = [];
        let args = Self::positional_args(&clause.args);
        let mut chfmt = fmt.clone();
        let mut body = String::new();

        let flags = self.add_flags(
            &cmd.flags,
            &cmd.sigils,
            completes,
            cmd.restart_token.as_deref(),
            &mut chfmt,
        );
        if !flags.is_empty() {
            chfmt.newline(&mut body);
            chfmt.indent(&mut body);
            body += &flags;
        }
        // The clause group repeats through its own separator, so the separator position
        // links back to this same parser. A restart token is a plain extra position.
        let mut links = ParserLinks {
            step_over: std::iter::once(separator.clone())
                .chain(cmd.restart_token.clone())
                .collect(),
            clause: Some((separator.clone(), clause_func_name.clone())),
            restart_token: cmd.restart_token.clone(),
            ..Default::default()
        };
        self.inherit_mount_step_over(&mut links);
        let links = self.generate_preserve_continuations(
            &clause_func_name,
            &args,
            completes,
            &links,
            &chfmt,
        );
        body += &Self::add_preserve_marker(&links);
        let main_args = self.generate_automatic_continuation(
            &clause_func_name,
            &args,
            (&cmd.flags, &cmd.sigils),
            completes,
            &links,
            &chfmt,
        );
        let args_body =
            self.add_args_and_cmds(&no_cmds, &main_args, completes, None, &links, &mut chfmt);
        if !args_body.is_empty() {
            chfmt.newline(&mut body);
            chfmt.indent(&mut body);
            body += &args_body;
        }

        let mut function = format!("function {clause_func_name}()\n    return clink.argmatcher()");
        function += &body;
        function += &Self::add_parser_policies(&main_args);
        function += Self::restart_loop(&links);
        if !function.ends_with('\n') {
            chfmt.newline(&mut function);
        }
        function += "end\n";
        self.cached_functions.insert(clause_func_name, function);
    }

    /// Wrap a sigil argument's completer and restore the prefix on each candidate.
    ///
    /// Caveat: Clink's environment-variable generator claims `%` words before an
    /// argmatcher can complete them, so `%` is not usable as a sigil on Windows.
    fn generate_sigil_arg_function(
        &mut self,
        arg: &cuc::usage::Arg,
        complete: &cuc::usage::Complete,
        restart_token: Option<&str>,
    ) {
        let sigil = arg.sigil.clone().unwrap_or_default();
        let func_name = Self::sigil_func_name(arg, complete);
        let inner = match &complete.kind {
            cuc::usage::CompleteKind::File => {
                "clink.filematches(value:sub(#sigil + 1))".to_string()
            }
            cuc::usage::CompleteKind::Dir => "clink.dirmatches(value:sub(#sigil + 1))".to_string(),
            cuc::usage::CompleteKind::Run(_) => {
                self.generate_arg_complete_function(complete);
                format!(
                    "{}(value:sub(#sigil + 1), word_index, line_state, match_builder, user_data)",
                    Self::function_ref(namespace::arg_complete_func_name(&complete.name))
                )
            }
            cuc::usage::CompleteKind::None => unreachable!("empty completers are filtered out"),
        };
        let function = format!(
            r#"function {func_name}(word, word_index, line_state, match_builder, user_data)
--[[
{sigil}{repr}
--]]
    local sigil = [[{sigil}]]
{restart_guard}    local value = sigil_word(word, word_index, line_state)
    if value:sub(1, #sigil) ~= sigil then
        return {{}}
    end
    local matches = {inner} or {{}}
    local prefixed = {{}}
    for _, match in ipairs(matches) do
        if type(match) == "table" then
            if match.match:sub(1, #sigil) ~= sigil then
                match.match = sigil .. match.match
            end
        elseif match:sub(1, #sigil) ~= sigil then
            match = sigil .. match
        end
        table.insert(prefixed, match)
    end
    return prefixed
end
"#,
            func_name = func_name,
            sigil = sigil,
            repr = arg.repr,
            inner = inner,
            restart_guard = restart_token
                .map(|token| format!(
                    "    if token_seen_before(\"{token}\", word_index, line_state) then return {{}} end\n"
                ))
                .unwrap_or_default(),
        );
        self.cached_functions.insert(func_name, function);
    }

    fn generate_arg_complete_function(&mut self, complete: &cuc::usage::Complete) {
        assert!(
            self.completor.is_some(),
            "No completor! Can't generate arg completions without it"
        );
        let mut function = String::new();
        let func_name = Self::function_ref(namespace::arg_complete_func_name(&complete.name));

        let complete_run = complete.kind.run();
        assert!(
            complete_run.is_some(),
            "complete wasn't of kind: run!\n{:?}",
            complete
        );
        let complete_run = complete_run.unwrap();
        let encoded_script = mbase64::encode(complete_run);

        let filter = match complete.descs {
            false => String::from("false"),
            true => String::from("true"),
        };

        function += "function ";
        function += &func_name;
        function += format!(
            r#"(word, word_index, line_state, match_builder, user_data)
--[[
{}
--]]
    local b64_encoded_script = [[{}]]
    return completor(word_index, line_state, b64_encoded_script, {})
"#,
            complete_run, encoded_script, filter,
        )
        .as_str();
        function += "end\n";

        self.cached_functions.insert(func_name, function);
    }

    fn find_arg_complete<'a>(
        &'a self,
        arg: &cuc::usage::Arg,
        completes: &'a HashMap<String, cuc::usage::Complete>,
    ) -> Option<&'a cuc::usage::Complete> {
        let arg_name_lower = arg.name.to_lowercase();
        completes
            .get(&arg_name_lower)
            .or_else(|| self.spec.completes.get(&arg_name_lower))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuc::usage::{
        Arg, Cmd, Complete, CompleteKind, DoubleDash, Flag, GlobalFlag, Info, PendingMount,
        UsageSpec,
    };

    fn generate(spec: UsageSpec) -> String {
        let mut functions = HashMap::new();
        GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate()
    }

    #[test]
    fn links_unknown_first_word_to_default_subcommand() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "run".into(),
                args: vec![Arg {
                    name: "task".into(),
                    repr: "[task]".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            default_subcommand: Some("run".into()),
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("onlink = function(link) return link or _cuc._cmd_run() end"));
    }

    #[test]
    fn default_subcommand_flags_continue_in_the_child_parser() {
        let child = Cmd {
            name: "run".into(),
            args: vec![Arg {
                name: "task".into(),
                repr: "<task>".into(),
                choices: vec!["task".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            flags: vec![
                Flag {
                    name: "fast".into(),
                    names: vec!["--fast".into()],
                    link_to: Some("run".into()),
                    ..Default::default()
                },
                Flag {
                    name: "jobs".into(),
                    names: vec!["--jobs".into()],
                    arg: Some(Arg {
                        name: "count".into(),
                        repr: "<count>".into(),
                        choices: vec!["1".into()],
                        ..Default::default()
                    }),
                    link_to: Some("run".into()),
                    ..Default::default()
                },
            ],
            cmds: vec![child],
            default_subcommand: Some("run".into()),
            default_subcommand_flags: true,
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains(r#""--fast" .. _cuc._cmd_run()"#));
        assert!(lua.contains("onlink = function(_,_,word,wi,_,ud) return _cuc._cmd_run() end"));
    }

    #[test]
    fn emits_double_dash_parser_policies() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            args: vec![Arg {
                name: "args".into(),
                repr: "[-- args]…".into(),
                var: true,
                min: Some(0),
                max: Some(-1),
                double_dash: DoubleDash::Required,
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        // `--` gates the required argument and is preserved within its variadic matcher.
        assert!(lua.contains("ud.double_dash_started"));
        assert!(lua.contains("double_dash_seen(wi,ls)"));
        assert!(lua.contains("ud.double_dash_preserve=true"));
        assert!(lua.contains(":setendofflags()"));
        assert!(!lua.contains(":setflagsanywhere(false)"));
    }

    #[test]
    fn maps_automatic_double_dash_to_setflagsanywhere() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            args: vec![Arg {
                name: "args".into(),
                repr: "[args]…".into(),
                var: true,
                min: Some(0),
                max: Some(-1),
                double_dash: DoubleDash::Automatic,
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains(":setflagsanywhere(false)"));
        assert!(lua.contains(":setendofflags()"));
    }

    #[test]
    fn starts_automatic_policy_at_its_argument() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            flags: vec![Flag {
                name: "option".into(),
                names: vec!["--option".into()],
                ..Default::default()
            }],
            args: vec![
                Arg {
                    name: "first".into(),
                    repr: "<first>".into(),
                    ..Default::default()
                },
                Arg {
                    name: "rest".into(),
                    repr: "[rest]…".into(),
                    var: true,
                    min: Some(0),
                    max: Some(-1),
                    double_dash: DoubleDash::Automatic,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("function _cuc._cmd_root_automatic()"));
        assert!(
            lua.contains(
                "onlink = function(_,_,word,wi,_,ud) return _cuc._cmd_root_automatic() end"
            )
        );
        let root = lua.rsplit("clink.argmatcher(\"demo\")").next().unwrap();
        assert!(!root.contains(":setflagsanywhere(false)"));
    }

    #[test]
    fn preserves_double_dash_tokens_for_preserve_arguments() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            args: vec![Arg {
                name: "args".into(),
                repr: "[args]…".into(),
                var: true,
                min: Some(0),
                max: Some(-1),
                double_dash: DoubleDash::Preserve,
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("ud.double_dash_preserve=true"));
        assert!(lua.contains("function _cuc._cmd_root_preserve_0()"));
        assert!(lua.contains(r#":_addexflags({{ "--" .. _cuc._cmd_root_preserve_0() }})"#));
        assert!(!lua.contains(":setendofflags()"));
    }

    #[test]
    fn uses_command_scoped_completers() {
        let mut command = Cmd {
            name: "open".into(),
            args: vec![Arg {
                name: "target".into(),
                repr: "[target]".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        command.completes.insert(
            "target".into(),
            Complete {
                name: "target".into(),
                kind: CompleteKind::Dir,
                descs: false,
            },
        );
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![command],
            ..Default::default()
        };

        assert!(generate(spec).contains("clink.dirmatches"));
    }

    #[test]
    fn emits_fixed_sigil_candidates_with_their_prefix() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            // A sigil is classified by its prefix, not by position, so its declared
            // candidates are matched anywhere flags are, each carrying the prefix.
            sigils: vec![Arg {
                name: "tools".into(),
                repr: "[tools]…".into(),
                choices: vec!["node@22".into(), "node@24".into()],
                sigil: Some("+".into()),
                var: true,
                min: Some(0),
                max: Some(-1),
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains(r#"{ "+node@22", " [tools]…" }"#));
        assert!(lua.contains(r#"{ "+node@24", " [tools]…" }"#));
        // One entry per candidate: later values in a table are a description, not choices.
        assert!(!lua.contains(r#"{ "+node@22", "+node@24""#));
    }

    #[test]
    fn suppresses_fixed_sigils_after_a_restart_token() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            sigils: vec![Arg {
                name: "tools".into(),
                repr: "[tools]…".into(),
                choices: vec!["node".into()],
                sigil: Some("+".into()),
                ..Default::default()
            }],
            restart_token: Some(":::".into()),
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains(r#"{ "+node", hide=true }"#));
        assert!(
            lua.contains(
                r#"if token_seen_before(":::", word_index, line_state) then return {} end"#
            )
        );
        assert!(lua.contains(r#"{ match = "+node", description = " [tools]…" }"#));
    }

    #[test]
    fn sigil_value_completer_restores_the_prefix_and_yields_to_other_words() {
        let mut completes = HashMap::new();
        completes.insert(
            "pkg".to_string(),
            Complete {
                name: "pkg".into(),
                kind: CompleteKind::Run("echo alpha".into()),
                descs: false,
            },
        );
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            completes,
            cmds: vec![Cmd {
                name: "add".into(),
                sigils: vec![Arg {
                    name: "pkg".into(),
                    repr: "[#pkg]".into(),
                    sigil: Some("#".into()),
                    ..Default::default()
                }],
                args: vec![Arg {
                    name: "pkg".into(),
                    repr: "[#pkg]".into(),
                    sigil: Some("#".into()),
                    ..Default::default()
                }],
                restart_token: Some(":::".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: Some(&Completor {
                exe_path: PathBuf::from("cuc.exe"),
                shell: PathBuf::from("bash"),
            }),
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate();

        assert!(lua.contains("function _cuc._sigil_arg_pkg_u3a__u3a_pkg_u3a__u3a__u23_("));
        assert!(lua.contains("match.match = sigil .. match.match"));
        assert!(lua.contains("match = sigil .. match"));
        // A word without the prefix steps over the sigil position.
        assert!(
            lua.contains(r##"if sigil_word(word, wi, ls):sub(1, 1) ~= "#" then return 1 end"##)
        );
        // The word under the cursor is not the empty `word` argument.
        assert!(lua.contains("local value = sigil_word(word, word_index, line_state)"));
        assert!(lua.contains("if value:sub(1, #sigil) ~= sigil then"));
        assert!(
            lua.contains(
                r#"if token_seen_before(":::", word_index, line_state) then return {} end"#
            )
        );
    }

    #[test]
    fn keeps_dynamic_sigil_positions_without_complete_generation() {
        let sigil = Arg {
            name: "pkg".into(),
            repr: "[#pkg]".into(),
            sigil: Some("#".into()),
            ..Default::default()
        };
        let mut completes = HashMap::new();
        completes.insert(
            "pkg".into(),
            Complete {
                name: "pkg".into(),
                kind: CompleteKind::Run("echo alpha".into()),
                descs: false,
            },
        );
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            args: vec![
                Arg {
                    name: "plain".into(),
                    repr: "[plain]".into(),
                    ..Default::default()
                },
                sigil.clone(),
            ],
            sigils: vec![sigil],
            completes,
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("Argument expected: [#pkg] [prefix: #]"));
        assert!(
            lua.contains(r##"if sigil_word(word, wi, ls):sub(1, 1) ~= "#" then return 1 end"##)
        );
        assert!(!lua.contains("function _sigil_arg_"));
    }

    #[test]
    fn links_commands_that_only_have_fixed_sigil_choices() {
        let sigil = Arg {
            name: "tool".into(),
            repr: "[tool]".into(),
            choices: vec!["node".into()],
            sigil: Some("+".into()),
            ..Default::default()
        };
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "add".into(),
                args: vec![sigil.clone()],
                sigils: vec![sigil],
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains(r#"{ "add" .. _cuc._cmd_add()"#));
        assert!(lua.contains(r#"{ "+node", " [tool]" }"#));
    }

    #[test]
    fn restart_token_repeats_the_positional_positions_without_a_fixed_limit() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "run".into(),
                args: vec![Arg {
                    name: "task".into(),
                    repr: "[task]".into(),
                    choices: vec!["alpha".into(), "beta".into()],
                    ..Default::default()
                }],
                restart_token: Some(":::".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        // The token is its own position, and `loop(1)` starts another invocation from it,
        // so no parser has to link back to itself and restarts are not bounded.
        assert!(lua.contains("\":::\""));
        assert!(lua.contains(":loop(1)"));
        // Every positional steps over the token, so it can restart at any position.
        assert!(lua.contains(r#"if word == ":::" then return 1 end"#));
        assert!(!lua.contains("_restart"));
    }

    #[test]
    fn emits_clause_flags_and_its_repeated_positional() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "use".into(),
                flags: vec![Flag {
                    name: "postinstall".into(),
                    names: vec!["--postinstall".into()],
                    arg: Some(Arg {
                        name: "COMMAND".into(),
                        repr: "<COMMAND>".into(),
                        required: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                // An implicit clause repeats its only positional, which the model turns
                // into one optional variadic argument hinting the clause synopsis.
                args: vec![Arg {
                    name: "TOOL@VERSION".into(),
                    repr: "[TOOL@VERSION]…".into(),
                    var: true,
                    min: Some(0),
                    max: Some(-1),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("--postinstall"));
        assert!(lua.contains("[TOOL@VERSION]…"));
        assert!(lua.contains("ud.var_min=0; ud.var_max=-1"));
    }

    #[test]
    fn separator_clause_links_back_to_an_unbounded_group_parser() {
        let args = vec![
            Arg {
                name: "left".into(),
                repr: "<left>".into(),
                choices: vec!["a".into()],
                ..Default::default()
            },
            Arg {
                name: "right".into(),
                repr: "<right>".into(),
                choices: vec!["b".into()],
                ..Default::default()
            },
        ];
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "pairs".into(),
                args: args.clone(),
                clause: Some(cuc::usage::Clause {
                    separator: Some(":::".into()),
                    args,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        assert!(lua.contains("function _cuc._cmd_pairs_clause()"));
        assert!(lua.matches("return _cuc._cmd_pairs_clause()").count() >= 2);
        assert!(lua.contains(
            r#"if word == ":::" and not double_dash_seen(wi,ls) then return _cuc._cmd_pairs_clause() end"#
        ));
        assert!(
            lua.contains(r#"if not double_dash_seen(wi,ls) then return {":::"} end return {}"#)
        );
        assert!(!lua.contains(",,"));
    }

    #[test]
    fn ignores_mounts_without_the_runtime_completor() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "run".into(),
                pending_mounts: vec![PendingMount {
                    run: "mise tasks --usage".into(),
                    synopsis: Some("[TASK]".into()),
                    overrides_default: false,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        // A mount can only be resolved by running it, which is what the completor does,
        // so without --complete the mounted commands are simply absent.
        let lua = generate(spec);
        assert!(!lua.contains("_mount_bind_"));
        assert!(!lua.contains("_complete_arg_mount"));
        assert!(lua.contains("{ \"run\""));
    }

    #[test]
    fn mount_mode_returns_an_initializer_instead_of_registering_a_command() {
        let spec = UsageSpec {
            info: Info {
                name: "plugin".into(),
                bin: "plugin".into(),
            },
            args: vec![Arg {
                name: "value".into(),
                repr: "[value]".into(),
                double_dash: DoubleDash::Preserve,
                ..Default::default()
            }],
            cmds: vec![Cmd {
                name: "nested".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
            mount_prefix: Some("parent"),
            mount_step_over: &[],
        }
        .generate();

        assert!(lua.contains("return function(collector, line_state)"));
        assert!(lua.contains("collector:begin_mount(false, true)"));
        assert!(lua.contains("function _cuc._cmd_parent_nested()"));
        assert!(lua.contains("{ \"nested\" .. _cuc._cmd_parent_nested()"));
        assert!(!lua.contains("clink.argmatcher(\"plugin\")"));
    }

    #[test]
    fn mounted_commands_yield_to_the_parent_restart_token() {
        let spec = UsageSpec {
            info: Info {
                name: "tasks".into(),
                bin: "tasks".into(),
            },
            cmds: vec![Cmd {
                name: "alpha".into(),
                args: vec![Arg {
                    name: "args".into(),
                    repr: "[ARGS]…".into(),
                    hide: true,
                    var: true,
                    min: Some(0),
                    max: Some(-1),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let step_over = vec![":::".to_string()];
        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
            mount_prefix: Some("run_mount_0"),
            mount_step_over: &step_over,
        }
        .generate();

        assert!(lua.contains(r#"if word == ":::" then return 1 end"#));
        assert!(!lua.contains("Argument expected: [ARGS]"));
        assert!(lua.contains("function() return {} end"));
        assert!(!lua.contains("{\n\t\t\t,"));
        assert!(!lua.contains(":loop(1)"));

        let mut loader_functions = HashMap::new();
        let loader = GeneratorView {
            spec: &spec,
            cached_functions: &mut loader_functions,
            completor: Some(&Completor {
                exe_path: PathBuf::from("cuc.exe"),
                shell: PathBuf::from("bash"),
            }),
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .mount_loader_body(
            &[PendingMount {
                run: "mise tasks --usage".into(),
                synopsis: None,
                overrides_default: false,
            }],
            &cuc::namespace::NameSpace::root(),
            "collector",
            &step_over,
            "",
        );
        let encoded = loader
            .split_once("base64.decode([[")
            .unwrap()
            .1
            .split_once("]]")
            .unwrap()
            .0;
        let script = mbase64::decode(encoded).unwrap();
        assert!(script.contains("--mount-step-over=':::'"));
    }

    #[test]
    fn default_subcommand_mounts_activate_without_the_command_word() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            default_subcommand: Some("run".into()),
            cmds: vec![
                Cmd {
                    name: "run".into(),
                    restart_token: Some(":::".into()),
                    pending_mounts: vec![PendingMount {
                        run: "mise tasks --usage".into(),
                        synopsis: None,
                        overrides_default: false,
                    }],
                    ..Default::default()
                },
                Cmd {
                    name: "install".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: Some(&Completor {
                exe_path: PathBuf::from("cuc.exe"),
                shell: PathBuf::from("bash"),
            }),
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate();

        assert!(lua.contains("{[ [[install]] ] = true}, true) then return end"));
        assert!(lua.contains("{[ [[run]] ] = true}"));
    }

    #[test]
    fn keeps_command_nested_mounts_lazy_in_mount_chunks() {
        let spec = UsageSpec {
            info: Info {
                name: "mounted".into(),
                bin: "mounted".into(),
            },
            cmds: vec![Cmd {
                name: "container".into(),
                pending_mounts: vec![PendingMount {
                    run: "plugin usage".into(),
                    synopsis: Some("[PLUGIN]".into()),
                    overrides_default: false,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut functions = HashMap::new();
        let arg_matchers = vec!["demo".to_string()];
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: Some(&Completor {
                exe_path: PathBuf::from("cuc.exe"),
                shell: PathBuf::from("bash"),
            }),
            arg_matchers: &arg_matchers,
            mount_prefix: Some("parent"),
            mount_step_over: &[],
        }
        .generate();

        assert!(lua.contains("local mount_context_generator = clink.generator(23)"));
        assert!(lua.contains("function _cuc._mount_bind_parent_u3a__u3a_container"));
        assert!(lua.contains("[ [[demo]] ] = true"));
        assert!(lua.contains("initialize(collector, line_state)"));
        assert!(lua.contains("for _, update in ipairs(mount_context_updaters)"));
    }

    #[test]
    fn turns_dynamic_mounts_into_runtime_completers() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            cmds: vec![Cmd {
                name: "run".into(),
                mount_prefix_flags: vec![Flag {
                    name: "cd".into(),
                    names: vec!["-C".into(), "--cd".into()],
                    global: GlobalFlag::Imposed(cuc::namespace::NameSpace::root()),
                    arg: Some(Arg::default()),
                    ..Default::default()
                }],
                pending_mounts: vec![
                    PendingMount {
                        run: "mise tasks --usage".into(),
                        synopsis: Some("[TASK] [ARGS]…".into()),
                        overrides_default: false,
                    },
                    PendingMount {
                        run: "other usage".into(),
                        synopsis: Some("[OTHER]…".into()),
                        overrides_default: false,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: Some(&Completor {
                exe_path: PathBuf::from("cuc.exe"),
                shell: PathBuf::from("bash"),
            }),
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate();

        // A pre-argmatcher generator updates only mounted command paths before the static
        // argmatcher generator reads them.
        assert!(lua.contains("local mount_context_generator = clink.generator(23)"));
        assert!(lua.contains("function collector:setendofflags(value)"));
        assert!(lua.contains("matcher:setendofflags()"));
        assert!(lua.contains("function _cuc._mount_bind_run(matcher, track_context)"));
        assert!(lua.contains(
            "mount_path_reached(line_state, {[ [[demo]] ] = true}, {[ [[run]] ] = true}, {}, false)"
        ));
        assert!(lua.contains("mount_context_args(line_state, {[ [[run]] ] = true}"));
        assert!(lua.contains("[ [[-C]] ] = true"));
        assert!(lua.contains("[ [[--cd]] ] = true"));
        assert!(lua.contains("script_source:gsub(\"__CUC_MOUNT_CONTEXT_ARGS__\""));
        assert!(lua.contains("return _cuc._mount_bind_run(clink.argmatcher())"));
        assert_eq!(lua.matches("local script_path = os.tmpname()").count(), 2);
        // Context changes reset and rebuild the static and mounted matcher definition.
        assert!(lua.contains("local context = os.getcwd() .. \"\\0\" .. mount_args"));
        assert!(lua.contains("if _mount_context_run == context then return end"));
        assert!(lua.contains("matcher:reset()"));
    }
}

#[cfg(test)]
mod lua_identifier_tests {
    use super::*;
    use cuc::usage::{Arg, Flag, Info, UsageSpec};

    #[test]
    fn slugifies_flag_names_used_in_lua_function_identifiers() {
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            flags: vec![Flag {
                name: "fs-events".into(),
                names: vec!["--fs-events".into()],
                arg: Some(Arg {
                    name: "events".into(),
                    repr: "<events>".into(),
                    choices: vec!["create".into(), "modify".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut functions = HashMap::new();
        let lua = GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate();

        assert!(lua.contains("local _cuc = {}"));
        assert!(lua.contains("function _cuc._flag_fs_u2d_events()"));
        assert!(!lua.contains("function _flag_"));
    }
}

#[cfg(test)]
mod global_flag_tests {
    use super::*;
    use cuc::namespace::NameSpace;
    use cuc::usage::{Arg, Cmd, Flag, GlobalFlag, Info, UsageSpec};

    fn generate(spec: UsageSpec) -> String {
        let mut functions = HashMap::new();
        GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
            mount_prefix: None,
            mount_step_over: &[],
        }
        .generate()
    }

    /// A command nested below a namespace that declares no global flags of its own
    /// must still link the global flags it inherits from further up the tree.
    #[test]
    fn links_inherited_globals_across_namespaces_without_own_globals() {
        let inherited = Flag {
            name: "quiet".into(),
            names: vec!["--quiet".into()],
            global: GlobalFlag::Itself,
            ..Default::default()
        };
        let spec = UsageSpec {
            info: Info {
                name: "demo".into(),
                bin: "demo".into(),
            },
            flags: vec![inherited.clone()],
            cmds: vec![Cmd {
                name: "a".into(),
                flags: vec![Flag {
                    global: GlobalFlag::Imposed(NameSpace::root().join("a")),
                    ..inherited.clone()
                }],
                cmds: vec![Box::new(Cmd {
                    name: "b".into(),
                    flags: vec![Flag {
                        global: GlobalFlag::Imposed(NameSpace::root().join("a")),
                        ..inherited
                    }],
                    args: vec![Arg {
                        name: "target".into(),
                        repr: "[target]".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                })],
                ..Default::default()
            }],
            ..Default::default()
        };

        let lua = generate(spec);
        let nested = lua
            .split("function _cuc._cmd_a_b()")
            .nth(1)
            .expect("nested command function should be generated");

        assert!(nested.contains("_cuc._global_flags_()"));
        // `_global_flags_a` is never defined, so referencing it would crash at runtime.
        assert!(!nested.contains("_cuc._global_flags_a("));
    }
}
