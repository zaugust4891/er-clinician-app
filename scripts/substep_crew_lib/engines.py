"""Read-only Claude/Codex adapters, tool-free Ollama adapter, and role profiles."""

from __future__ import annotations

import json
import os
import shutil
import time
import urllib.request
from pathlib import Path

from . import config, processes, protocol


class Engine:
    name = "engine"
    tools = False

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> processes.Result:
        raise NotImplementedError


def _invocation_final_path(log_path: Path | None, suffix: str) -> Path:
    if log_path is None:
        config.RUNS.mkdir(parents=True, exist_ok=True)
        return config.RUNS / f"standalone-{os.getpid()}-{time.time_ns()}.final{suffix}"
    name = log_path.name.removesuffix(".transcript.log")
    return log_path.with_name(f"{name}.final{suffix}")


class ClaudeEngine(Engine):
    """Read-only Claude CLI; JSON actions are mediated by the harness."""

    tools = True

    def __init__(
        self,
        model: str = "opus",
        permission_mode: str = "dontAsk",
        config_dir: Path | None = None,
    ):
        self.model, self.permission_mode = model, permission_mode
        self.config_dir = config_dir or config.default_claude_config_dir()
        self.name = f"claude:{model}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> processes.Result:
        if not shutil.which("claude"):
            return processes.Result(False, "", 0.0, "claude CLI not found", log_path)
        argv = [
            "claude", "-p",
            "--model", self.model,
            "--permission-mode", self.permission_mode,
            "--output-format", "json" if schema else "text",
            "--tools", "Read,Glob,Grep",
            "--disallowedTools", "mcp__*",
            "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
            "--safe-mode", "--disable-slash-commands",
            "--settings", '{"disableAllHooks":true}',
            "--append-system-prompt",
            "Never run git commit, git push, git stash, git reset, git checkout --, "
            "or any command that discards working-tree changes. Leave all changes "
            "unstaged and uncommitted for human review.",
        ]
        if schema is not None:
            argv += ["--json-schema", json.dumps(json.loads(schema.read_text(encoding="utf-8")))]
        final_path = _invocation_final_path(log_path, ".json" if schema else ".md")
        final_path.unlink(missing_ok=True)
        res = processes.run_process(
            argv,
            timeout,
            "claude",
            env={
                "CLAUDE_CONFIG_DIR": str(self.config_dir),
                "PAI_DIR": str(self.config_dir),
            },
            log_path=log_path,
            stdout_path=final_path,
            input_text=prompt,
        )
        try:
            # Claude print mode writes its result on stdout. Capturing it
            # separately prevents stderr diagnostics in the combined transcript
            # from corrupting reviewer JSON. Result.output is the complete final
            # response, unlike run_process's bounded combined-output excerpt.
            res.output = final_path.read_text(encoding="utf-8")
            if schema is not None:
                envelope = protocol.load_json(res.output)
                if isinstance(envelope, dict) and envelope.get("type") == "result":
                    if (
                        envelope.get("is_error") is not False
                        or envelope.get("subtype") != "success"
                        or "structured_output" not in envelope
                    ):
                        raise ValueError("Claude did not return successful structured output")
                    res.output = json.dumps(envelope["structured_output"], ensure_ascii=False)
        except (OSError, UnicodeError, ValueError) as exc:
            res.ok = False
            res.note = f"cannot preserve Claude final response: {exc}"
        return res


class CodexEngine(Engine):
    """Tool-using Codex CLI. Default reviewer sandbox is read-only."""

    tools = True

    def __init__(
        self,
        model: str | None = None,
        sandbox: str = "read-only",
        oss: bool = False,
        local_provider: str = "ollama",
    ):
        self.model, self.sandbox, self.oss, self.local_provider = (
            model, sandbox, oss, local_provider,
        )
        self.name = f"codex:{'oss/' if oss else ''}{model or 'default'}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> processes.Result:
        if not shutil.which("codex"):
            return processes.Result(False, "", 0.0, "codex CLI not found", log_path)
        if self.sandbox != "read-only":
            return processes.Result(False, "", 0.0, "patch sessions require the read-only Codex sandbox", log_path)
        final_path = _invocation_final_path(log_path, ".json" if schema else ".md")
        final_path.parent.mkdir(parents=True, exist_ok=True)
        final_path.unlink(missing_ok=True)
        argv = ["codex", "exec", "--cd", str(config.REPO), "-s", self.sandbox, "--color", "never"]
        # Integrations and hooks can execute outside the shell sandbox. Replace
        # their entire configuration tables, rather than trusting prompt rules.
        argv += ["--ignore-rules"]
        for override in (
            'approval_policy="never"', "mcp_servers={}", "plugins={}",
            "hooks={}", "notify=[]", "features.apps=false",
        ):
            argv += ["-c", override]
        if self.oss:
            argv += ["--oss", "--local-provider", self.local_provider]
        if self.model:
            argv += ["-m", self.model]
        if schema:
            argv += ["--output-schema", str(schema)]
        argv += ["-o", str(final_path), "-"]
        res = processes.run_process(argv, timeout, "codex", log_path=log_path, input_text=prompt)
        if final_path.exists():
            try:
                res.output = final_path.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as exc:
                res.ok = False
                res.note = f"cannot read Codex final response: {exc}"
        elif res.ok:
            res.ok = False
            res.note = "Codex did not save a final response"
        return res


class OllamaEngine(Engine):
    """Tool-free local chat for condensing logs; never editing."""

    tools = False

    def __init__(
        self,
        model: str = "qwen3.5:4b",
        host: str = config.OLLAMA,
        num_ctx: int = 16384,
        num_predict: int = 1024,
    ):
        self.model, self.host = model, host
        self.num_ctx, self.num_predict = num_ctx, num_predict
        self.name = f"ollama:{model}"

    def run(
        self,
        prompt: str,
        timeout: int,
        schema: Path | None = None,
        *,
        log_path: Path | None = None,
    ) -> processes.Result:
        payload = {
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            "think": False,
            "options": {
                "temperature": 0.1,
                "num_ctx": self.num_ctx,
                "num_predict": self.num_predict,
            },
        }
        if schema:
            payload["format"] = json.loads(schema.read_text(encoding="utf-8"))
        req = urllib.request.Request(
            f"{self.host}/api/chat",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        start = time.monotonic()
        resolved = log_path.resolve() if log_path is not None else None
        try:
            with urllib.request.urlopen(req, timeout=timeout) as response:
                raw = response.read()
            if resolved is not None:
                resolved.parent.mkdir(parents=True, exist_ok=True)
                resolved.write_bytes(raw)
            body = json.loads(raw.decode("utf-8"))
            output = body["message"]["content"]
            if not isinstance(output, str):
                raise TypeError("message.content is not a string")
            if resolved is not None:
                final_path = _invocation_final_path(resolved, ".json" if schema else ".md")
                final_path.write_text(output, encoding="utf-8")
            return processes.Result(True, output, time.monotonic() - start, log_path=resolved)
        except (urllib.error.URLError, OSError, KeyError, TypeError, UnicodeError, json.JSONDecodeError) as exc:
            return processes.Result(False, "", time.monotonic() - start, f"ollama unavailable: {exc}", resolved)


PROFILES: dict[str, dict[str, object]] = {
    # All authors request writes through the same reviewed-patch boundary.
    "default": {
        "implementer": lambda: ClaudeEngine("opus"),
        "reviewer": lambda: CodexEngine(sandbox="read-only"),
        "condenser": lambda: OllamaEngine(),
    },
    # everything local — costs nothing, use it to debug this script's wiring
    "local": {
        "implementer": lambda: CodexEngine(
            model="qwen3.5:4b", sandbox="read-only", oss=True
        ),
        "reviewer": lambda: CodexEngine(model="qwen3.5:4b", sandbox="read-only", oss=True),
        "condenser": lambda: OllamaEngine(),
    },
    "claude-only": {
        "implementer": lambda: ClaudeEngine("opus"),
        "reviewer": lambda: ClaudeEngine("opus", permission_mode="plan"),
        "condenser": lambda: OllamaEngine(),
    },
}
