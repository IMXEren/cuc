use std::{borrow::Borrow, collections::HashMap, path::PathBuf};

use cuc::namespace;

use super::formatter::GenFormatter;
use crate::{mbase64, string::StringExt};

#[derive(Default)]
pub struct Completor {
    pub exe_path: PathBuf,
    pub shell: PathBuf,
}

/// How many further restarts a generated argument chain supports. Each level links its
/// restart token to the level below it, because a Clink parser link has to point at an
/// already-built parser; a chain cannot link to itself.
const RESTART_LEVELS: usize = 3;

/// A usage `restart_token`: the token that begins another invocation of the same command.
#[derive(Clone)]
struct Restart {
    token: String,
    /// Index of the generated chain the token continues with.
    level: usize,
    /// Function name of the chain this token belongs to, e.g. `_cmd_run`.
    prefix: String,
}

impl Restart {
    /// Name of the generated chain that parses the invocation following the token.
    fn next_func_name(&self) -> String {
        format!("{}_restart_{}", self.prefix, self.level)
    }
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
}

impl GeneratorView<'_> {
    pub fn generate(&mut self) -> String {
        let mut fmt = GenFormatter::default();

        let mut script_start = r#"require("arghelper")
local base64 = require("base64")

function loop_until(word_index, line_state, user_data)
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

-- `word` is empty for the word under the cursor, and that word reports a zero length,
-- so read from its offset to the end of the line: a prefix check only needs its start.
function sigil_word(word, word_index, line_state)
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

        if let Some(completor) = self.completor {
            script_start += &format!(
                r#"function completor(word_index, line_state, b64_encoded_script, filter)
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

        let mut script_body = {
            if self.arg_matchers.is_empty() {
                format!("\nclink.argmatcher(\"{}\")", self.spec.info.bin)
            } else {
                "\nlocal matcher = clink.argmatcher()".to_string()
            }
        };

        fmt.newline(&mut script_body);
        fmt.indent(&mut script_body);

        let sigils = self.spec.sigils.clone();
        let body = self.add_flags(&self.spec.flags, &sigils, &self.spec.completes, &mut fmt);
        if !body.is_empty() {
            script_body += &body;
            fmt.newline(&mut script_body);
            fmt.indent(&mut script_body);
        }

        let (pending_mounts, completes) = (
            self.spec.pending_mounts.clone(),
            self.spec.completes.clone(),
        );
        let (mount_args, mount_completes) =
            self.dynamic_mount_args(&pending_mounts, &completes, &fmt.ns);
        let mut args = mount_args;
        args.extend(self.spec.args.iter().cloned());
        let args = Self::positional_args(&args);
        let body = self.add_args_and_cmds(
            &self.spec.cmds,
            &args,
            &mount_completes,
            self.spec.default_subcommand.as_deref(),
            None,
            &mut fmt,
        );
        if !body.is_empty() {
            script_body += &body;
            fmt.newline(&mut script_body);
            fmt.indent(&mut script_body);
        }

        script_body += &Self::add_parser_policies(&self.spec.args);
        script_body += ":nofiles()";

        if !self.arg_matchers.is_empty() {
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

    fn add_flags(
        &mut self,
        flags: &[cuc::usage::Flag],
        sigils: &[cuc::usage::Arg],
        completes: &HashMap<String, cuc::usage::Complete>,
        fmt: &mut GenFormatter,
    ) -> String {
        // Generate functions of returning anonymous clink.argmatcher
        // to link them to the corresponding flag
        self.generate_flag_functions(flags, completes, fmt);
        // A sigil is matched anywhere flags are, so its literal candidates belong in the
        // flag list rather than in a positional slot.
        let sigil_bodies = self.sigil_flag_bodies(sigils);
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
                    .map(|ancestor| ancestor.global_flag_func_name())
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
    fn sigil_flag_bodies(&self, sigils: &[cuc::usage::Arg]) -> Vec<String> {
        let mut bodies = Vec::new();
        for arg in sigils.iter().filter(|arg| arg.sigil.is_some()) {
            if arg.choices.is_empty() {
                continue;
            }
            let sigil = arg.sigil.clone().unwrap_or_default();
            for choice in arg.choices.iter() {
                // One entry per candidate: a table's later values are a description, not
                // more alternatives.
                let mut body = format!("{{ \"{sigil}{choice}\"");
                if !arg.repr.is_empty() {
                    body += &format!(", \" {}\"", arg.repr.replace('"', "\\\""));
                }
                body += " }";
                bodies.push(body);
            }
        }
        bodies
    }

    fn sigil_func_name(arg: &cuc::usage::Arg, complete: &cuc::usage::Complete) -> String {
        namespace::sigil_arg_func_name(format!(
            "{}::{}::{}",
            complete.name,
            arg.name,
            arg.sigil.as_deref().unwrap_or_default()
        ))
    }

    fn add_flag_body(&self, flag: &cuc::usage::Flag, fmt: &GenFormatter) -> String {
        let ns = fmt.ns.view();
        let mut completions = String::new();
        let flag_name = namespace::slugify(&flag.name);
        let func_name = ns.flag_func_name(&flag_name);
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

    fn add_arg_loop_until(arg: &cuc::usage::Arg, restart_token: Option<&str>) -> String {
        let mut body = Vec::new();
        // A sigil argument never takes the ordinary positional cursor: it is classified by
        // its prefix, so a word without the prefix steps over this position.
        if let Some(sigil) = &arg.sigil {
            body.push(format!(
                "if sigil_word(word, wi, ls):sub(1, {}) ~= \"{sigil}\" then return 1 end",
                sigil.len()
            ));
        }
        // The token that restarts this command's arguments is parsed by the position that
        // follows, so this position must step over it instead of taking it as a value.
        if let Some(token) = restart_token {
            body.push(format!("if word == \"{token}\" then return 1 end"));
        }
        if arg.double_dash == cuc::usage::DoubleDash::Preserve {
            body.push("ud.double_dash_preserve=true".to_string());
        }
        if arg.double_dash == cuc::usage::DoubleDash::Required {
            body.push(
                "if not ud.double_dash_started then if ls:getword(wi-1) ~= \"--\" then return 1 end; ud.double_dash_started=true end"
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

    fn add_arg_close(arg: Option<&cuc::usage::Arg>, restart_token: Option<&str>) -> String {
        let mut completions = String::new();
        if let Some(arg) = arg {
            completions += &Self::add_arg_hint(arg);
            completions += &Self::add_arg_loop_until(arg, restart_token);
        }
        completions += "})";
        completions
    }

    /// Clink defaults to recognizing flags anywhere. `double_dash=automatic` means
    /// flags stop being recognized once a positional value has been entered, which
    /// is exactly what `setflagsanywhere(false)` does.
    fn add_parser_policies(args: &[cuc::usage::Arg]) -> String {
        if args
            .iter()
            .any(|arg| arg.double_dash == cuc::usage::DoubleDash::Automatic)
        {
            ":setflagsanywhere(false)".to_string()
        } else {
            String::new()
        }
    }

    fn enclose_arg(
        matcher: Option<&str>,
        arg: &cuc::usage::Arg,
        restart_token: Option<&str>,
    ) -> String {
        let mut completion = Self::add_arg_start();
        if let Some(matcher) = matcher.filter(|matcher| !matcher.is_empty()) {
            completion += matcher;
            completion += ", "; // Adding ',' because required by hint
        }
        completion += &Self::add_arg_close(Some(arg), restart_token);
        completion
    }

    /// @param enclose: add start and close to string
    fn add_arg(
        &mut self,
        arg: &cuc::usage::Arg,
        completes: &HashMap<String, cuc::usage::Complete>,
        enclose: bool,
        restart_token: Option<&str>,
    ) -> String {
        let matcher = if arg.sigil.is_some() && arg.choices.is_empty() {
            // A dynamic sigil needs a wrapper that restores the prefix. Even when
            // --complete is absent, its empty position remains so onadvance can skip it.
            self.find_arg_complete(arg, completes)
                .cloned()
                .filter(|complete| {
                    matches!(complete.kind, cuc::usage::CompleteKind::Run(_))
                        && self.completor.is_some()
                })
                .map(|complete| {
                    self.generate_sigil_arg_function(arg, &complete);
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
                        Some(namespace::arg_complete_func_name(&complete.name))
                    }
                    _ => None,
                })
        };

        if enclose {
            Self::enclose_arg(matcher.as_deref(), arg, restart_token)
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
        restart: Option<Restart>,
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
        let last_arg = args.len().checked_sub(1);
        let restart_token = restart.as_ref().map(|restart| restart.token.as_str());

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
            *completions += &Self::add_arg_close(None, None);
        };

        if !args.is_empty() {
            arg = Some(args[0].borrow());
            let arg = arg.unwrap();
            let token = if Some(0) == last_arg {
                restart_token
            } else {
                None
            };
            let arg_completion = self.add_arg(arg, completes, false, token);
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
            let func_name = fmt.ns.view().cmd_func_name(&cmd_name);

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
            let function_name = fmt.ns.view().cmd_func_name(&command_name);
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
                let token = if last_arg == Some(0) {
                    restart_token
                } else {
                    None
                };
                completions += &Self::add_arg_loop_until(arg, token);
            }

            entry_close(&mut completions, fmt);
        }

        if args.len() > 1 {
            for (index, arg) in args[1..].iter().enumerate() {
                let arg = arg.borrow();
                fmt.newline(&mut completions);
                fmt.indent(&mut completions);
                let token = if Some(index + 1) == last_arg {
                    restart_token
                } else {
                    None
                };
                completions += &self.add_arg(arg, completes, true, token);
            }
        }

        if let Some(restart) = restart {
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &Self::add_arg_start();
            fmt.increment_level();
            fmt.newline(&mut completions);
            fmt.indent(&mut completions);
            completions += &format!(
                "{{ \"{}\" .. {}() }}",
                restart.token,
                restart.next_func_name()
            );
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
            let func_name = ns.flag_func_name(&flag_name);
            let mut function = String::new();
            if let Some(ref arg) = flag.arg {
                let arg_completion = self.add_arg(arg, completes, true, None);
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

            let func_name = ns.global_flag_func_name();
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
            let func_name = fmt.ns.view().cmd_func_name(&cmd_name);

            let subcmds = cmd.cmds.as_slice();
            let (mount_args, mount_completes) =
                self.dynamic_mount_args(&cmd.pending_mounts, &cmd.completes, &chfmt.ns);
            let mut args = mount_args;
            args.extend(cmd.args.iter().cloned());
            let args = Self::positional_args(&args);
            if !cmd.flags.is_empty()
                || !cmd.sigils.is_empty()
                || !subcmds.is_empty()
                || !args.is_empty()
            {
                let mut cmd_completion = String::new();

                let completion =
                    self.add_flags(&cmd.flags, &cmd.sigils, &cmd.completes, &mut chfmt);
                if !completion.is_empty() {
                    fmt.newline(&mut cmd_completion);
                    fmt.indent(&mut cmd_completion);
                    cmd_completion += &completion;
                }

                let restart = cmd.restart_token.as_ref().map(|token| Restart {
                    token: token.clone(),
                    level: RESTART_LEVELS - 1,
                    prefix: func_name.clone(),
                });
                let completion = self.add_args_and_cmds(
                    subcmds,
                    &args,
                    &mount_completes,
                    None,
                    restart.clone(),
                    &mut chfmt,
                );
                if !completion.is_empty() {
                    fmt.newline(&mut cmd_completion);
                    fmt.indent(&mut cmd_completion);
                    cmd_completion += &completion;
                }

                function += "function ";
                function += &func_name;
                function += r#"()
    return clink.argmatcher()"#;
                function += &cmd_completion;
                function += &Self::add_parser_policies(&args);
                if !function.ends_with("\n") {
                    fmt.newline(&mut function);
                }
                function += "end\n";

                if restart.is_some() {
                    self.generate_restart_functions(
                        cmd,
                        &func_name,
                        &args,
                        &mount_completes,
                        &chfmt,
                    );
                }
            }

            self.cached_functions.insert(func_name, function);
        }
    }

    /// Sigil arguments with fixed candidates are matched anywhere through the flag list, so
    /// they must not also take a positional slot.
    fn positional_args(args: &[cuc::usage::Arg]) -> Vec<cuc::usage::Arg> {
        args.iter()
            .filter(|arg| arg.sigil.is_none() || arg.choices.is_empty())
            .cloned()
            .collect()
    }

    /// Generate the chains that parse the invocations following a `restart_token`. Each level
    /// parses one more invocation of the command and links its own token one level down,
    /// because a Clink parser link has to point at an already-built parser: a chain cannot
    /// link to itself, so the repetition is bounded instead of cyclic.
    fn generate_restart_functions(
        &mut self,
        cmd: &cuc::usage::Cmd,
        func_name: &str,
        args: &[cuc::usage::Arg],
        completes: &HashMap<String, cuc::usage::Complete>,
        fmt: &GenFormatter,
    ) {
        let Some(token) = cmd.restart_token.as_ref() else {
            return;
        };
        let func_name = func_name.to_string();
        let no_cmds: [cuc::usage::Cmd; 0] = [];
        for level in 0..RESTART_LEVELS {
            let level_func_name = format!("{func_name}_restart_{level}");
            let mut chfmt = fmt.clone();
            let mut body = String::new();

            let flags = self.add_flags(&cmd.flags, &cmd.sigils, completes, &mut chfmt);
            if !flags.is_empty() {
                chfmt.newline(&mut body);
                chfmt.indent(&mut body);
                body += &flags;
            }

            // A restart resets the positional cursor, not the subcommand routing, so the
            // command's subcommands are not offered again here.
            let restart = (level > 0).then(|| Restart {
                token: token.clone(),
                level: level - 1,
                prefix: func_name.clone(),
            });
            let args_body =
                self.add_args_and_cmds(&no_cmds, args, completes, None, restart, &mut chfmt);
            if !args_body.is_empty() {
                chfmt.newline(&mut body);
                chfmt.indent(&mut body);
                body += &args_body;
            }

            let mut function = String::new();
            function += "function ";
            function += &level_func_name;
            function += r#"()
    return clink.argmatcher()"#;
            function += &body;
            function += &Self::add_parser_policies(args);
            if !function.ends_with("\n") {
                chfmt.newline(&mut function);
            }
            function += "end\n";
            self.cached_functions.insert(level_func_name, function);
        }
    }

    /// Wrap an argument's runtime completer so that every candidate carries the sigil back:
    /// usage removes the prefix before completing and restores it on each candidate.
    ///
    /// Caveat: Clink's environment-variable generator claims `%` words before an
    /// argmatcher can complete them, so `%` is not usable as a sigil on Windows.
    fn generate_sigil_arg_function(
        &mut self,
        arg: &cuc::usage::Arg,
        complete: &cuc::usage::Complete,
    ) {
        assert!(
            self.completor.is_some(),
            "No completor! Can't generate sigil arg completions without it"
        );
        self.generate_arg_complete_function(complete);

        let sigil = arg.sigil.clone().unwrap_or_default();
        let inner = namespace::arg_complete_func_name(&complete.name);
        let func_name = Self::sigil_func_name(arg, complete);
        let function = format!(
            r#"function {func_name}(word, word_index, line_state, match_builder, user_data)
--[[
{sigil}{repr}
--]]
    local sigil = [[{sigil}]]
    if sigil_word(word, word_index, line_state):sub(1, #sigil) ~= sigil then
        return {{}}
    end
    local matches = {inner}(word, word_index, line_state, match_builder, user_data) or {{}}
    local prefixed = {{}}
    for _, match in ipairs(matches) do
        if match:sub(1, #sigil) == sigil then
            table.insert(prefixed, match)
        else
            table.insert(prefixed, sigil .. match)
        end
    end
    return prefixed
end
"#,
            func_name = func_name,
            sigil = sigil,
            repr = arg.repr,
            inner = inner,
        );
        self.cached_functions.insert(func_name, function);
    }

    fn generate_arg_complete_function(&mut self, complete: &cuc::usage::Complete) {
        assert!(
            self.completor.is_some(),
            "No completor! Can't generate arg completions without it"
        );
        let mut function = String::new();
        let func_name = namespace::arg_complete_func_name(&complete.name);

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

    /// Represent all mounts at a command as alternatives in one positional slot. Each
    /// mount remains dynamic: its `run` command executes in the completion-time working
    /// directory and is piped through `cuc subcommands`. Without `--complete`, mounts are
    /// deliberately omitted rather than snapshotted during generation. Only mounted command
    /// names are exposed; the mounted trees cannot be linked without resolving them eagerly.
    fn dynamic_mount_args(
        &mut self,
        pending_mounts: &[cuc::usage::PendingMount],
        completes: &HashMap<String, cuc::usage::Complete>,
        namespace: &cuc::namespace::NameSpace,
    ) -> (Vec<cuc::usage::Arg>, HashMap<String, cuc::usage::Complete>) {
        let mut completes = completes.clone();
        let Some(completor) = self.completor else {
            return (Vec::new(), completes);
        };
        if pending_mounts.is_empty() {
            return (Vec::new(), completes);
        }

        let subcommands = format!("\"{}\" subcommands", completor.exe_path.display());
        let run = pending_mounts
            .iter()
            .map(|mount| format!("{} | {subcommands}", mount.run))
            .collect::<Vec<_>>()
            .join("; ");
        let name = if namespace.is_root() {
            "mount".to_string()
        } else {
            format!("{}::mount", namespace.display())
        };
        let complete = cuc::usage::Complete {
            name: name.clone(),
            kind: cuc::usage::CompleteKind::Run(run),
            // Descriptions use ':' as a separator, but mounted names may contain it.
            descs: false,
        };
        self.generate_arg_complete_function(&complete);

        let repr = match pending_mounts {
            [mount] => mount
                .synopsis
                .clone()
                .unwrap_or_else(|| "[COMMANDS]…".to_string()),
            _ => "[COMMANDS]…".to_string(),
        };
        let args = vec![cuc::usage::Arg {
            name: name.clone(),
            repr,
            ..Default::default()
        }];
        completes.insert(name.to_lowercase(), complete);
        (args, completes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuc::usage::{
        Arg, Cmd, Complete, CompleteKind, DoubleDash, Flag, Info, PendingMount, UsageSpec,
    };

    fn generate(spec: UsageSpec) -> String {
        let mut functions = HashMap::new();
        GeneratorView {
            spec: &spec,
            cached_functions: &mut functions,
            completor: None,
            arg_matchers: &Vec::new(),
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
        assert!(lua.contains("onlink = function(link) return link or _cmd_run() end"));
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
        // `--` both gates a required argument and ends an unbounded variadic one.
        assert!(lua.contains("ud.double_dash_started"));
        assert!(lua.contains("prev_word == \"--\""));
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
        assert!(!lua.contains(":setendofflags()"));
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

        assert!(generate(spec).contains("ud.double_dash_preserve=true"));
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
        }
        .generate();

        assert!(lua.contains("function _sigil_arg_pkg_u3a__u3a_pkg_u3a__u3a__u23_("));
        assert!(lua.contains("table.insert(prefixed, sigil .. match)"));
        // A word without the prefix steps over the sigil position.
        assert!(
            lua.contains(r##"if sigil_word(word, wi, ls):sub(1, 1) ~= "#" then return 1 end"##)
        );
        // The word under the cursor is not the empty `word` argument.
        assert!(lua.contains("sigil_word(word, word_index, line_state):sub(1, #sigil)"));
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
        assert!(lua.contains(r#"{ "add" .. _cmd_add()"#));
        assert!(lua.contains(r#"{ "+node", " [tool]" }"#));
    }

    #[test]
    fn restart_token_links_a_bounded_chain_that_resets_the_positional_cursor() {
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
        // Each level continues with the one below it, because a parser link cannot be a cycle.
        assert!(lua.contains("function _cmd_run_restart_2()"));
        assert!(lua.contains(r#"{ ":::" .. _cmd_run_restart_2() }"#));
        assert!(lua.contains(r#"{ ":::" .. _cmd_run_restart_0() }"#));
        // The last declared argument steps over the token so the next invocation parses it.
        assert!(lua.contains(r#"if word == ":::" then return 1 end"#));
        // The deepest level accepts no further token.
        let deepest = lua
            .split("function _cmd_run_restart_0()")
            .nth(1)
            .expect("deepest level");
        assert!(!deepest.contains("::: \" .."));
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
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        // A mount can only be resolved by running it, which is what the completor does,
        // so without --complete the mounted commands are simply absent.
        let lua = generate(spec);
        assert!(!lua.contains("subcommands"));
        assert!(!lua.contains("_complete_arg_mount"));
        assert!(lua.contains("{ \"run\""));
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
                pending_mounts: vec![
                    PendingMount {
                        run: "mise tasks --usage".into(),
                        synopsis: Some("[TASK] [ARGS]…".into()),
                    },
                    PendingMount {
                        run: "other usage".into(),
                        synopsis: Some("[OTHER]…".into()),
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
        }
        .generate();

        // The mount command runs while completing, piped through `cuc subcommands`.
        assert!(lua.contains("mise tasks --usage | \"cuc.exe\" subcommands"));
        assert!(lua.contains("other usage | \"cuc.exe\" subcommands"));
        assert!(lua.contains("function _complete_arg_run_u3a__u3a_mount"));
        // All mounts share one command-name position and one combined completer.
        assert_eq!(lua.matches("_complete_arg_run_u3a__u3a_mount").count(), 2);
        assert!(lua.contains("Argument expected: [COMMANDS]…"));
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
        }
        .generate();

        assert!(lua.contains("function _flag_fs_u2d_events()"));
        assert!(!lua.contains("function _flag_fs-events()"));
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
            .split("function _cmd_a_b()")
            .nth(1)
            .expect("nested command function should be generated");

        assert!(nested.contains("_global_flags_()"));
        // `_global_flags_a` is never defined, so referencing it would crash at runtime.
        assert!(!nested.contains("_global_flags_a("));
    }
}
