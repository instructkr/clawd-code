# commands crate

## OVERVIEW

REPL slash-command surface: parsing, spec registry, help rendering, and a handful of in-crate handlers. Single flat `src/lib.rs` (~7k lines). Deps: `plugins`, `runtime`, `serde_json` only. Note: `tools` depends on `commands`, not the reverse.

## lib.rs MAP

| Lines | Landmark |
|-------------|----------|
| 16–58 | Registry types: `CommandManifestEntry`, `CommandSource` (Builtin / InternalOnly / FeatureGated), `CommandRegistry`, `SlashCommandSpec`, `SkillSlashDispatch` |
| 60–1047 | `SLASH_COMMAND_SPECS` static table. 120+ entries (help, status, sandbox, compact, model, permissions, clear, cost, resume, config, mcp, memory, init, diff, version, bughunter, commit, pr, issue, ultraplan, teleport, debug-tool-call, export, session, plugin, agents, skills, doctor, plan, review, tasks, theme, vim, voice, chat, ...) |
| 1048–1303 | `SlashCommand` enum (~65 variants + `Unknown(String)`), `SlashCommandParseError`, `SlashCommand::parse` (L1218) |
| 1515–1899 | Per-command arg parsers: `parse_mcp_command`, `parse_plugin_command`, `parse_session_command`, etc. |
| 1900–2108 | Help/suggestion rendering: `render_slash_command_help*`, `suggest_slash_commands` (Levenshtein), category grouping |
| 2109–2682 | Result types + handlers: `handle_plugins_slash_command`, `handle_agents/mcp/skills_slash_command(_json)`, skill dispatch/resolve |
| 3160–5293 | Reporting layer: paired text and `_json` renderers for plugins/agents/skills/mcp reports, skill install/uninstall/create-agent logic, frontmatter parsing, root discovery |
| 5294 | `handle_slash_command(input, session, compaction)` top dispatch. Only Compact and Help execute here; all other variants return to the REPL caller |
| 5403–7183 | `mod tests` (~1780 lines) |

## ADDING A SLASH COMMAND

1. **Spec.** Add a `SlashCommandSpec` entry to `SLASH_COMMAND_SPECS`. Set `resume_supported` honestly.
2. **Enum + parse.** Add a variant to `SlashCommand`. Wire a match arm in `SlashCommand::parse`. If the command takes arguments, add a dedicated `parse_*_command` function in the arg-parser block.
3. **Handler.** Decide where execution lives:
   - In-crate (like Compact/Help): handle it inside `handle_slash_command`.
   - Returned to caller: just return the parsed variant. The REPL layer executes it.
4. **Help.** Make sure the spec's `summary` and `argument_hint` are set so help rendering and suggestion matching pick it up automatically.
5. **Tests.** Cover parsing (valid input, bad input, edge cases) in the inline `mod tests`.

## CONVENTIONS

- **Dual renderers.** Every report surface has a text variant and a `_json` variant: `handle_x` / `handle_x_json`, `render_*` / `render_*_json`. Keep them in sync.
- **Error style.** Handlers return `std::io::Result`. Parse failures use `SlashCommandParseError`.
- **Manifest registries.** Pattern is `entries: Vec<_Entry>` backed by the static spec table.
- **Dependency direction.** This crate knows nothing about `tools`. Don't import it.
- **No execution here.** Almost all commands pass through as parsed data. Only Compact and Help run inside this crate. Respect that boundary.
