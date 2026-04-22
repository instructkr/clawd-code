from __future__ import annotations

import argparse

from .bootstrap_graph import build_bootstrap_graph
from .command_graph import build_command_graph
from .commands import execute_command, get_command, get_commands, render_command_index
from .direct_modes import run_deep_link, run_direct_connect
from .parity_audit import run_parity_audit
from .permissions import ToolPermissionContext
from .port_manifest import build_port_manifest
from .query_engine import QueryEnginePort
from .remote_runtime import run_remote_mode, run_ssh_mode, run_teleport_mode
from .runtime import PortRuntime
from .session_store import (
    SessionDeleteError,
    SessionNotFoundError,
    delete_session,
    list_sessions,
    load_session,
    session_exists,
)
from .setup import run_setup
from .tool_pool import assemble_tool_pool
from .tools import execute_tool, get_tool, get_tools, render_tool_index


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description='Python porting workspace for the Claude Code rewrite effort')
    subparsers = parser.add_subparsers(dest='command', required=True)
    subparsers.add_parser('summary', help='render a Markdown summary of the Python porting workspace')
    subparsers.add_parser('manifest', help='print the current Python workspace manifest')
    subparsers.add_parser('parity-audit', help='compare the Python workspace against the local ignored TypeScript archive when available')
    subparsers.add_parser('setup-report', help='render the startup/prefetch setup report')
    command_graph_parser = subparsers.add_parser('command-graph', help='show command graph segmentation')
    command_graph_parser.add_argument('--output-format', choices=['text', 'json'], default='text')
    tool_pool_parser = subparsers.add_parser('tool-pool', help='show assembled tool pool with default settings')
    tool_pool_parser.add_argument('--output-format', choices=['text', 'json'], default='text')
    subparsers.add_parser('bootstrap-graph', help='show the mirrored bootstrap/runtime graph stages')
    list_parser = subparsers.add_parser('subsystems', help='list the current Python modules in the workspace')
    list_parser.add_argument('--limit', type=int, default=32)

    commands_parser = subparsers.add_parser('commands', help='list mirrored command entries from the archived snapshot')
    commands_parser.add_argument('--limit', type=int, default=20)
    commands_parser.add_argument('--query')
    commands_parser.add_argument('--no-plugin-commands', action='store_true')
    commands_parser.add_argument('--no-skill-commands', action='store_true')

    tools_parser = subparsers.add_parser('tools', help='list mirrored tool entries from the archived snapshot')
    tools_parser.add_argument('--limit', type=int, default=20)
    tools_parser.add_argument('--query')
    tools_parser.add_argument('--simple-mode', action='store_true')
    tools_parser.add_argument('--no-mcp', action='store_true')
    tools_parser.add_argument('--deny-tool', action='append', default=[])
    tools_parser.add_argument('--deny-prefix', action='append', default=[])

    route_parser = subparsers.add_parser('route', help='route a prompt across mirrored command/tool inventories')
    route_parser.add_argument('prompt')
    route_parser.add_argument('--limit', type=int, default=5)
    # #168: parity with show-command/show-tool/session-lifecycle CLI family
    route_parser.add_argument('--output-format', choices=['text', 'json'], default='text')

    bootstrap_parser = subparsers.add_parser('bootstrap', help='build a runtime-style session report from the mirrored inventories')
    bootstrap_parser.add_argument('prompt')
    bootstrap_parser.add_argument('--limit', type=int, default=5)
    # #168: parity with CLI family
    bootstrap_parser.add_argument('--output-format', choices=['text', 'json'], default='text')

    loop_parser = subparsers.add_parser('turn-loop', help='run a small stateful turn loop for the mirrored runtime')
    loop_parser.add_argument('prompt')
    loop_parser.add_argument('--limit', type=int, default=5)
    loop_parser.add_argument('--max-turns', type=int, default=3)
    loop_parser.add_argument('--structured-output', action='store_true')
    loop_parser.add_argument(
        '--timeout-seconds',
        type=float,
        default=None,
        help='total wall-clock budget across all turns (#161). Default: unbounded.',
    )
    loop_parser.add_argument(
        '--continuation-prompt',
        default=None,
        help=(
            'prompt to submit on turns after the first (#163). Default: None '
            '(loop stops after turn 0). Replaces the deprecated implicit "[turn N]" '
            'suffix that used to pollute the transcript.'
        ),
    )

    flush_parser = subparsers.add_parser(
        'flush-transcript',
        help='persist and flush a temporary session transcript (#160/#166: claw-native session API)',
    )
    flush_parser.add_argument('prompt')
    flush_parser.add_argument(
        '--directory', help='session storage directory (default: .port_sessions)'
    )
    flush_parser.add_argument(
        '--output-format',
        choices=['text', 'json'],
        default='text',
        help='output format',
    )
    flush_parser.add_argument(
        '--session-id',
        help='deterministic session ID (default: auto-generated UUID)',
    )

    load_session_parser = subparsers.add_parser(
        'load-session',
        help='load a previously persisted session (#160/#165: claw-native session API)',
    )
    load_session_parser.add_argument('session_id')
    load_session_parser.add_argument(
        '--directory', help='session storage directory (default: .port_sessions)'
    )
    load_session_parser.add_argument(
        '--output-format',
        choices=['text', 'json'],
        default='text',
        help='output format',
    )

    list_sessions_parser = subparsers.add_parser(
        'list-sessions',
        help='enumerate stored session IDs (#160: claw-native session API)',
    )
    list_sessions_parser.add_argument(
        '--directory', help='session storage directory (default: .port_sessions)'
    )
    list_sessions_parser.add_argument(
        '--output-format',
        choices=['text', 'json'],
        default='text',
        help='output format',
    )

    delete_session_parser = subparsers.add_parser(
        'delete-session',
        help='delete a persisted session (#160: idempotent, race-safe)',
    )
    delete_session_parser.add_argument('session_id')
    delete_session_parser.add_argument(
        '--directory', help='session storage directory (default: .port_sessions)'
    )
    delete_session_parser.add_argument(
        '--output-format',
        choices=['text', 'json'],
        default='text',
        help='output format',
    )

    remote_parser = subparsers.add_parser('remote-mode', help='simulate remote-control runtime branching')
    remote_parser.add_argument('target')
    ssh_parser = subparsers.add_parser('ssh-mode', help='simulate SSH runtime branching')
    ssh_parser.add_argument('target')
    teleport_parser = subparsers.add_parser('teleport-mode', help='simulate teleport runtime branching')
    teleport_parser.add_argument('target')
    direct_parser = subparsers.add_parser('direct-connect-mode', help='simulate direct-connect runtime branching')
    direct_parser.add_argument('target')
    deep_link_parser = subparsers.add_parser('deep-link-mode', help='simulate deep-link runtime branching')
    deep_link_parser.add_argument('target')

    show_command = subparsers.add_parser('show-command', help='show one mirrored command entry by exact name')
    show_command.add_argument('name')
    show_command.add_argument('--output-format', choices=['text', 'json'], default='text')
    show_tool = subparsers.add_parser('show-tool', help='show one mirrored tool entry by exact name')
    show_tool.add_argument('name')
    show_tool.add_argument('--output-format', choices=['text', 'json'], default='text')

    exec_command_parser = subparsers.add_parser('exec-command', help='execute a mirrored command shim by exact name')
    exec_command_parser.add_argument('name')
    exec_command_parser.add_argument('prompt')
    # #168: parity with CLI family
    exec_command_parser.add_argument('--output-format', choices=['text', 'json'], default='text')

    exec_tool_parser = subparsers.add_parser('exec-tool', help='execute a mirrored tool shim by exact name')
    exec_tool_parser.add_argument('name')
    exec_tool_parser.add_argument('payload')
    # #168: parity with CLI family
    exec_tool_parser.add_argument('--output-format', choices=['text', 'json'], default='text')
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    manifest = build_port_manifest()
    if args.command == 'summary':
        print(QueryEnginePort(manifest).render_summary())
        return 0
    if args.command == 'manifest':
        print(manifest.to_markdown())
        return 0
    if args.command == 'parity-audit':
        print(run_parity_audit().to_markdown())
        return 0
    if args.command == 'setup-report':
        print(run_setup().as_markdown())
        return 0
    if args.command == 'command-graph':
        graph = build_command_graph()
        if args.output_format == 'json':
            import json
            envelope = {
                'builtins_count': len(graph.builtins),
                'plugin_like_count': len(graph.plugin_like),
                'skill_like_count': len(graph.skill_like),
                'total_count': len(graph.flattened()),
                'builtins': [{'name': m.name, 'source_hint': m.source_hint} for m in graph.builtins],
                'plugin_like': [{'name': m.name, 'source_hint': m.source_hint} for m in graph.plugin_like],
                'skill_like': [{'name': m.name, 'source_hint': m.source_hint} for m in graph.skill_like],
            }
            print(json.dumps(envelope))
        else:
            print(graph.as_markdown())
        return 0
    if args.command == 'tool-pool':
        pool = assemble_tool_pool()
        if args.output_format == 'json':
            import json
            envelope = {
                'simple_mode': pool.simple_mode,
                'include_mcp': pool.include_mcp,
                'tool_count': len(pool.tools),
                'tools': [{'name': t.name, 'source_hint': t.source_hint} for t in pool.tools],
            }
            print(json.dumps(envelope))
        else:
            print(pool.as_markdown())
        return 0
    if args.command == 'bootstrap-graph':
        print(build_bootstrap_graph().as_markdown())
        return 0
    if args.command == 'subsystems':
        for subsystem in manifest.top_level_modules[: args.limit]:
            print(f'{subsystem.name}\t{subsystem.file_count}\t{subsystem.notes}')
        return 0
    if args.command == 'commands':
        if args.query:
            print(render_command_index(limit=args.limit, query=args.query))
        else:
            commands = get_commands(include_plugin_commands=not args.no_plugin_commands, include_skill_commands=not args.no_skill_commands)
            output_lines = [f'Command entries: {len(commands)}', '']
            output_lines.extend(f'- {module.name} — {module.source_hint}' for module in commands[: args.limit])
            print('\n'.join(output_lines))
        return 0
    if args.command == 'tools':
        if args.query:
            print(render_tool_index(limit=args.limit, query=args.query))
        else:
            permission_context = ToolPermissionContext.from_iterables(args.deny_tool, args.deny_prefix)
            tools = get_tools(simple_mode=args.simple_mode, include_mcp=not args.no_mcp, permission_context=permission_context)
            output_lines = [f'Tool entries: {len(tools)}', '']
            output_lines.extend(f'- {module.name} — {module.source_hint}' for module in tools[: args.limit])
            print('\n'.join(output_lines))
        return 0
    if args.command == 'route':
        matches = PortRuntime().route_prompt(args.prompt, limit=args.limit)
        # #168: JSON envelope for machine parsing
        if args.output_format == 'json':
            import json
            envelope = {
                'prompt': args.prompt,
                'limit': args.limit,
                'match_count': len(matches),
                'matches': [
                    {
                        'kind': m.kind,
                        'name': m.name,
                        'score': m.score,
                        'source_hint': m.source_hint,
                    }
                    for m in matches
                ],
            }
            print(json.dumps(envelope))
            return 0
        if not matches:
            print('No mirrored command/tool matches found.')
            return 0
        for match in matches:
            print(f'{match.kind}\t{match.name}\t{match.score}\t{match.source_hint}')
        return 0
    if args.command == 'bootstrap':
        session = PortRuntime().bootstrap_session(args.prompt, limit=args.limit)
        # #168: JSON envelope for machine parsing
        if args.output_format == 'json':
            import json
            envelope = {
                'prompt': session.prompt,
                'limit': args.limit,
                'setup': {
                    'python_version': session.setup.python_version,
                    'implementation': session.setup.implementation,
                    'platform_name': session.setup.platform_name,
                    'test_command': session.setup.test_command,
                },
                'routed_matches': [
                    {
                        'kind': m.kind,
                        'name': m.name,
                        'score': m.score,
                        'source_hint': m.source_hint,
                    }
                    for m in session.routed_matches
                ],
                'command_execution_messages': list(session.command_execution_messages),
                'tool_execution_messages': list(session.tool_execution_messages),
                'turn': {
                    'prompt': session.turn_result.prompt,
                    'output': session.turn_result.output,
                    'stop_reason': session.turn_result.stop_reason,
                },
                'persisted_session_path': session.persisted_session_path,
            }
            print(json.dumps(envelope))
            return 0
        print(session.as_markdown())
        return 0
    if args.command == 'turn-loop':
        results = PortRuntime().run_turn_loop(
            args.prompt,
            limit=args.limit,
            max_turns=args.max_turns,
            structured_output=args.structured_output,
            timeout_seconds=args.timeout_seconds,
            continuation_prompt=args.continuation_prompt,
        )
        for idx, result in enumerate(results, start=1):
            print(f'## Turn {idx}')
            print(result.output)
            print(f'stop_reason={result.stop_reason}')
        # Exit 2 when a timeout terminated the loop so claws can distinguish
        # 'ran to completion' from 'hit wall-clock budget'.
        if results and results[-1].stop_reason == 'timeout':
            return 2
        return 0
    if args.command == 'flush-transcript':
        from pathlib import Path as _Path
        engine = QueryEnginePort.from_workspace()
        # #166: allow deterministic session IDs for claw checkpointing/replay.
        # When unset, the engine's auto-generated UUID is used (backward compat).
        if args.session_id:
            engine.session_id = args.session_id
        engine.submit_message(args.prompt)
        directory = _Path(args.directory) if args.directory else None
        path = engine.persist_session(directory)
        if args.output_format == 'json':
            import json as _json
            print(_json.dumps({
                'session_id': engine.session_id,
                'path': path,
                'flushed': engine.transcript_store.flushed,
                'messages_count': len(engine.mutable_messages),
                'input_tokens': engine.total_usage.input_tokens,
                'output_tokens': engine.total_usage.output_tokens,
            }))
        else:
            # #166: legacy text output preserved byte-for-byte for backward compat.
            print(path)
            print(f'flushed={engine.transcript_store.flushed}')
        return 0
    if args.command == 'load-session':
        from pathlib import Path as _Path
        directory = _Path(args.directory) if args.directory else None
        # #165: catch typed SessionNotFoundError + surface a JSON error envelope
        # matching the delete-session contract shape. No more raw tracebacks.
        try:
            session = load_session(args.session_id, directory)
        except SessionNotFoundError as exc:
            if args.output_format == 'json':
                import json as _json
                resolved_dir = str(directory) if directory else '.port_sessions'
                print(_json.dumps({
                    'session_id': args.session_id,
                    'loaded': False,
                    'error': {
                        'kind': 'session_not_found',
                        'message': str(exc),
                        'directory': resolved_dir,
                        'retryable': False,
                    },
                }))
            else:
                print(f'error: {exc}')
            return 1
        except (OSError, ValueError) as exc:
            # Corrupted session file, IO error, JSON decode error — distinct
            # from 'not found'. Callers may retry here (fs glitch).
            if args.output_format == 'json':
                import json as _json
                resolved_dir = str(directory) if directory else '.port_sessions'
                print(_json.dumps({
                    'session_id': args.session_id,
                    'loaded': False,
                    'error': {
                        'kind': 'session_load_failed',
                        'message': str(exc),
                        'directory': resolved_dir,
                        'retryable': True,
                    },
                }))
            else:
                print(f'error: {exc}')
            return 1
        if args.output_format == 'json':
            import json as _json
            print(_json.dumps({
                'session_id': session.session_id,
                'loaded': True,
                'messages_count': len(session.messages),
                'input_tokens': session.input_tokens,
                'output_tokens': session.output_tokens,
            }))
        else:
            print(f'{session.session_id}\n{len(session.messages)} messages\nin={session.input_tokens} out={session.output_tokens}')
        return 0
    if args.command == 'list-sessions':
        from pathlib import Path as _Path
        directory = _Path(args.directory) if args.directory else None
        ids = list_sessions(directory)
        if args.output_format == 'json':
            import json as _json
            print(_json.dumps({'sessions': ids, 'count': len(ids)}))
        else:
            if not ids:
                print('(no sessions)')
            else:
                for sid in ids:
                    print(sid)
        return 0
    if args.command == 'delete-session':
        from pathlib import Path as _Path
        directory = _Path(args.directory) if args.directory else None
        try:
            deleted = delete_session(args.session_id, directory)
        except SessionDeleteError as exc:
            if args.output_format == 'json':
                import json as _json
                print(_json.dumps({
                    'session_id': args.session_id,
                    'deleted': False,
                    'error': {
                        'kind': 'session_delete_failed',
                        'message': str(exc),
                        'retryable': True,
                    },
                }))
            else:
                print(f'error: {exc}')
            return 1
        if args.output_format == 'json':
            import json as _json
            print(_json.dumps({
                'session_id': args.session_id,
                'deleted': deleted,
                'status': 'deleted' if deleted else 'not_found',
            }))
        else:
            if deleted:
                print(f'deleted: {args.session_id}')
            else:
                print(f'not found: {args.session_id}')
        # Exit 0 for both cases — delete_session is idempotent,
        # not-found is success from a cleanup perspective
        return 0
    if args.command == 'remote-mode':
        print(run_remote_mode(args.target).as_text())
        return 0
    if args.command == 'ssh-mode':
        print(run_ssh_mode(args.target).as_text())
        return 0
    if args.command == 'teleport-mode':
        print(run_teleport_mode(args.target).as_text())
        return 0
    if args.command == 'direct-connect-mode':
        print(run_direct_connect(args.target).as_text())
        return 0
    if args.command == 'deep-link-mode':
        print(run_deep_link(args.target).as_text())
        return 0
    if args.command == 'show-command':
        module = get_command(args.name)
        if module is None:
            if args.output_format == 'json':
                import json
                error_envelope = {
                    'name': args.name,
                    'found': False,
                    'error': {
                        'kind': 'command_not_found',
                        'message': f'Unknown command: {args.name}',
                        'retryable': False,
                    },
                }
                print(json.dumps(error_envelope))
            else:
                print(f'Command not found: {args.name}')
            return 1
        if args.output_format == 'json':
            import json
            output = {
                'name': module.name,
                'found': True,
                'source_hint': module.source_hint,
                'responsibility': module.responsibility,
            }
            print(json.dumps(output))
        else:
            print('\n'.join([module.name, module.source_hint, module.responsibility]))
        return 0
    if args.command == 'show-tool':
        module = get_tool(args.name)
        if module is None:
            if args.output_format == 'json':
                import json
                error_envelope = {
                    'name': args.name,
                    'found': False,
                    'error': {
                        'kind': 'tool_not_found',
                        'message': f'Unknown tool: {args.name}',
                        'retryable': False,
                    },
                }
                print(json.dumps(error_envelope))
            else:
                print(f'Tool not found: {args.name}')
            return 1
        if args.output_format == 'json':
            import json
            output = {
                'name': module.name,
                'found': True,
                'source_hint': module.source_hint,
                'responsibility': module.responsibility,
            }
            print(json.dumps(output))
        else:
            print('\n'.join([module.name, module.source_hint, module.responsibility]))
        return 0
    if args.command == 'exec-command':
        result = execute_command(args.name, args.prompt)
        # #168: JSON envelope with typed not-found error
        if args.output_format == 'json':
            import json
            if not result.handled:
                envelope = {
                    'name': args.name,
                    'prompt': args.prompt,
                    'handled': False,
                    'error': {
                        'kind': 'command_not_found',
                        'message': result.message,
                        'retryable': False,
                    },
                }
            else:
                envelope = {
                    'name': result.name,
                    'prompt': result.prompt,
                    'source_hint': result.source_hint,
                    'handled': True,
                    'message': result.message,
                }
            print(json.dumps(envelope))
        else:
            print(result.message)
        return 0 if result.handled else 1
    if args.command == 'exec-tool':
        result = execute_tool(args.name, args.payload)
        # #168: JSON envelope with typed not-found error
        if args.output_format == 'json':
            import json
            if not result.handled:
                envelope = {
                    'name': args.name,
                    'payload': args.payload,
                    'handled': False,
                    'error': {
                        'kind': 'tool_not_found',
                        'message': result.message,
                        'retryable': False,
                    },
                }
            else:
                envelope = {
                    'name': result.name,
                    'payload': result.payload,
                    'source_hint': result.source_hint,
                    'handled': True,
                    'message': result.message,
                }
            print(json.dumps(envelope))
        else:
            print(result.message)
        return 0 if result.handled else 1
    parser.error(f'unknown command: {args.command}')
    return 2


if __name__ == '__main__':
    raise SystemExit(main())
