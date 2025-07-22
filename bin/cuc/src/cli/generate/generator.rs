use std::{borrow::Borrow, collections::HashMap, path::PathBuf};

use cuc::namespace;

use super::formatter::GenFormatter;
use crate::{mbase64, string::StringExt};

#[derive(Default)]
pub struct Completor {
    pub exe_path: PathBuf,
    pub shell: PathBuf,
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

        let mut script_start = format!(
            r#"require("arghelper")

function loop_until(word_index, line_state, user_data)
	if not user_data.first_index then
		user_data.first_index = word_index
	end
	local diff = user_data.var_max - user_data.var_min
	local prev_word = line_state:getword(word_index - 1)
	-- var_max is -1 to loop forever and using '--' should break the loop
	-- to point to next arg position. 
	-- diff is used to loop limitedly till max but also break with '--' if greater than min
	if (user_data.var_max < 0 and prev_word == "--")
		or ((diff > 0 and word_index >= user_data.first_index + diff)
			or (prev_word == "--" and word_index >= user_data.first_index + user_data.var_min))
	then
		return 1
	end
	return 0
end

"#
        );

        let mut script_body = {
            if self.arg_matchers.is_empty() {
                format!("\nclink.argmatcher(\"{}\")", self.spec.info.bin)
            } else {
                format!("\nlocal matcher = clink.argmatcher()")
            }
        };

        fmt.newline(&mut script_body);
        fmt.indent(&mut script_body);

        let body = self.add_flags(&self.spec.flags, &mut fmt);
        if !body.is_empty() {
            script_body += &body;
            fmt.newline(&mut script_body);
            fmt.indent(&mut script_body);
        }

        let body = self.add_args_and_cmds(&self.spec.cmds, &self.spec.args, &mut fmt);
        if !body.is_empty() {
            script_body += &body;
            fmt.newline(&mut script_body);
            fmt.indent(&mut script_body);
        }

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

    fn add_flags(&mut self, flags: &[cuc::usage::Flag], fmt: &mut GenFormatter) -> String {
        // Generate functions of returning anonymous clink.argmatcher
        // to link them to the corresponding flag
        self.generate_flag_functions(flags, fmt);
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

        for (index, flag) in flags.iter().enumerate() {
            if index == 0 {
                entry_start(&mut completions, fmt);
            }

            // Add all non global flags
            if !flag.is_global() {
                let body = self.add_flag_body(flag, &fmt);
                if !body.is_empty() {
                    completions += &body;
                    completions += &entry_delim(&fmt);
                }
                found_non_global_flag = true;
            } else {
                found_global_flag = true;
            }
        }

        let ns = fmt.ns.view();
        if !flags.is_empty() {
            let entry_delim = entry_delim(&fmt);
            if found_global_flag {
                // Now, add the global flag funcs containing the flags of parent and itself.

                let parent_gfunc_name = ns.parent().global_flag_func_name();

                if found_non_global_flag && !completions.ends_with(&entry_delim) {
                    completions += &entry_delim;
                }
                completions += &parent_gfunc_name;
                completions += "()";

                let current_gfunc_name = ns.global_flag_func_name();
                // Incase of root ns, the parent global and current global flag funcs would be same
                if current_gfunc_name != parent_gfunc_name {
                    completions += &entry_delim;
                    completions += &format!("{} and {}()", current_gfunc_name, current_gfunc_name);
                }
            } else {
                // Trimming the entry_delim as to safely close
                completions.trim_end_matches_mut(&entry_delim);
            }

            entry_close(&mut completions, fmt);
        }
        completions
    }

    fn add_flag_body(&self, flag: &cuc::usage::Flag, fmt: &GenFormatter) -> String {
        let ns = fmt.ns.view();
        let mut completions = String::new();
        let func_name = ns.flag_func_name(&flag.name);
        let mut flag_names = flag.names.clone();
        flag.aliases
            .iter()
            .for_each(|alias| flag_names.push(alias.name.clone()));
        for (inner_index, name) in flag_names.iter().enumerate() {
            if inner_index != 0 {
                completions += ",";
                fmt.newline(&mut completions);
                fmt.indent(&mut completions);
                completions += "--[[alias]] ";
            }

            completions += "{ \"";
            completions += &name;
            completions += "\"";

            if let Some(function) = self.cached_functions.get(&func_name) {
                if !function.is_empty() {
                    completions += " .. ";
                    completions += &func_name;
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

    /// Expects the caller to add ',' (comma) to separate the hint from args.
    fn add_arg_hint(arg: &cuc::usage::Arg) -> String {
        let mut completions = String::from("hint = [===[Argument expected: ");
        completions += &arg.repr;
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

    fn add_arg_loop_until(arg: &cuc::usage::Arg) -> String {
        let mut completions = String::new();
        if arg.var {
            completions += ", ";
            // Instead of using :loop(), use onadvance to advance through the argument positions.
            // This is because one can't break out of loop to the next arg position.
            completions += &format!(
                "onadvance = function(_,_,wi,ls,ud) ud.var_min={}; ud.var_max={}; return loop_until(wi,ls,ud) end",
                arg.min.unwrap(),
                arg.max.unwrap(),
            )
        }
        completions
    }

    fn add_arg_close(arg: Option<&cuc::usage::Arg>) -> String {
        let mut completions = String::new();
        if let Some(arg) = arg {
            completions += &Self::add_arg_hint(arg);
            completions += &Self::add_arg_loop_until(arg);
        }
        completions += "})";
        completions
    }

    /// @param enclose: add start and close to string
    fn add_arg(&mut self, arg: &cuc::usage::Arg, enclose: bool) -> String {
        let mut completions = String::new();
        if !arg.choices.is_empty() {
            if enclose {
                completions += &Self::add_arg_start();
            }
            completions += "\"";
            completions += &arg.choices.join(r#"", ""#);
            completions += "\"";
            if enclose {
                completions += ", "; // Adding ',' because required by hint
                completions += &Self::add_arg_close(Some(arg));
            }
        } else if let Some(complete) = self.find_arg_complete(arg) {
            let complete = complete.clone();
            match complete.kind {
                cuc::usage::CompleteKind::File | cuc::usage::CompleteKind::Dir => {
                    if enclose {
                        completions += &Self::add_arg_start();
                    }
                    if complete.kind.is_file() {
                        completions += "clink.filematches";
                    } else {
                        completions += "clink.dirmatches";
                    }
                    if enclose {
                        completions += ", "; // Adding ',' because required by hint
                        completions += &Self::add_arg_close(Some(arg));
                    }
                }
                cuc::usage::CompleteKind::Run(_) if self.completor.is_some() => {
                    self.generate_arg_complete_function(&complete);
                    let func_name = namespace::arg_complete_func_name(&complete.name);
                    if let Some(function) = self.cached_functions.get(&func_name) {
                        if !function.is_empty() {
                            if enclose {
                                completions += &Self::add_arg_start();
                            }
                            completions += &func_name;
                            if enclose {
                                completions += ", "; // Adding ',' because required by hint
                                completions += &Self::add_arg_close(Some(arg));
                            }
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Just add input hints for helping
            if enclose {
                completions += &Self::add_arg_start();
            }
            if enclose {
                completions += &Self::add_arg_close(Some(arg));
            }
        }
        completions
    }

    fn add_args_and_cmds<C, A>(&mut self, cmds: &[C], args: &[A], fmt: &mut GenFormatter) -> String
    where
        C: Borrow<cuc::usage::Cmd>,
        A: Borrow<cuc::usage::Arg>,
    {
        // Generate functions to be linked with subcmds
        self.generate_cmd_functions(cmds, fmt);
        let mut completions = String::new();
        let mut arg: Option<&cuc::usage::Arg> = None;
        let mut started = false;

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
            *completions += &Self::add_arg_close(None);
        };

        if !args.is_empty() {
            arg = Some(args[0].borrow());
            let arg = arg.unwrap();
            let arg_completion = self.add_arg(arg, false);
            if !arg_completion.is_empty() {
                entry_start(&mut completions, fmt);
                completions += &arg_completion;
                // Flag to check if the addarg was started
                // Have to re-enable if no args but subcmds
                started = true;
            }
        }

        for cmd in cmds.iter() {
            let cmd: &cuc::usage::Cmd = cmd.borrow();
            let cmd_name = namespace::slugify(&cmd.name);
            let func_name = fmt.ns.view().cmd_func_name(&cmd_name);

            if !started {
                entry_start(&mut completions, fmt);
                started = true;
            } else {
                completions += &entry_delim(fmt);
            }

            let mut cmd_names = vec![&cmd.name];
            cmd.aliases
                .iter()
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

                let subcmds = cmd.cmds.as_slice();
                if !cmd.flags.is_empty() || !subcmds.is_empty() || !cmd.args.is_empty() {
                    if let Some(function) = self.cached_functions.get(&func_name) {
                        if !function.is_empty() {
                            completions += " .. ";
                            completions += &func_name;
                            completions += "()";
                        }
                    }
                }
                if !cmd.help.is_empty() {
                    completions += &format!(", [===[{}]===]", cmd.help);
                }
                completions += " }";
            }
        }

        if started {
            if let Some(arg) = arg {
                completions += &entry_delim(fmt);
                completions += &Self::add_arg_hint(arg);
                completions += &Self::add_arg_loop_until(arg);
            }

            entry_close(&mut completions, fmt);
        }

        if args.len() > 1 {
            for arg in args[1..].iter() {
                let arg = arg.borrow();
                fmt.newline(&mut completions);
                fmt.indent(&mut completions);
                completions += &self.add_arg(arg, true);
            }
        }

        completions
    }

    fn generate_flag_functions(&mut self, flags: &[cuc::usage::Flag], fmt: &GenFormatter) {
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

            let func_name = ns.flag_func_name(&flag.name);
            let mut function = String::new();
            if let Some(ref arg) = flag.arg {
                let arg_completion = self.add_arg(arg, true);
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
                body += &Self::add_flag_body(&self, gflag, &fmt);
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
            if !cmd.flags.is_empty() || !subcmds.is_empty() || !cmd.args.is_empty() {
                let mut cmd_completion = String::new();

                let completion = self.add_flags(&cmd.flags, &mut chfmt);
                if !completion.is_empty() {
                    fmt.newline(&mut cmd_completion);
                    fmt.indent(&mut cmd_completion);
                    cmd_completion += &completion;
                }

                let completion = self.add_args_and_cmds(subcmds, &cmd.args, &mut chfmt);
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
                if !function.ends_with("\n") {
                    fmt.newline(&mut function);
                }
                function += "end\n";
            }

            self.cached_functions.insert(func_name, function);
        }
    }

    fn generate_arg_complete_function(&mut self, complete: &cuc::usage::Complete) {
        assert!(
            self.completor.is_some(),
            "No completor! Can't generate arg completions without it"
        );
        let completor = self.completor.unwrap();
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

        let filter_descriptions_code = match complete.descs {
            false => String::new(),
            true => String::from(
                r#"line = line:match("^([^:]+):") -- for filtering out descriptions
        "#,
            ),
        };

        function += "function ";
        function += &func_name;
        function += format!(
            r#"(word, word_index, line_state, match_builder, user_data)
--[[
{}
--]]
    local b64_encoded_script = [[{}]]
    local exec = [[{}]]
    local shell = [[{}]]
    local args = [[ complete --current ]] .. word_index - 1 .. [[ --line "]] .. line_state:getline() .. [[" --shell "]] .. shell .. [[" -- "]] .. b64_encoded_script .. [["]]
    local pipe = io.popen(exec .. args)
    assert(pipe, "[ERROR]: failed to run complete command")
    local complete_args = {{}}
    for line in pipe:lines() do
        {}table.insert(complete_args, line)
    end
    pipe:close()
    return complete_args
"#,
            complete_run, encoded_script, completor.exe_path.display(), completor.shell.display(), filter_descriptions_code
        )
        .as_str();
        function += "end\n";

        self.cached_functions.insert(func_name, function);
    }

    fn find_arg_complete<'a>(
        &'a self,
        arg: &'a cuc::usage::Arg,
    ) -> Option<&'a cuc::usage::Complete> {
        let arg_name_lower = arg.name.to_lowercase();
        let complete: Option<&cuc::usage::Complete> = self.spec.completes.get(&arg_name_lower);
        complete
    }
}
