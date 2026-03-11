"""pexpect wrapper for interactive writ commands.

Handles interactive prompts from `writ init` (without --yes)
and `writ finish` (without --yes).
"""

from pathlib import Path
from typing import Dict, List, Optional, Tuple

import pexpect


class WritInteractive:
    """Drive interactive writ commands via pexpect."""

    def __init__(self, writ_bin: str, cwd: Path, timeout: int = 30):
        self.writ_bin = writ_bin
        self.cwd = str(cwd)
        self.timeout = timeout

    def init_interactive(
        self,
        answers: Optional[Dict[str, str]] = None,
    ) -> str:
        """Run writ init interactively, answering prompts.

        Args:
            answers: Dict of prompt substring -> answer string.
                     Unmatched prompts get 'y' or Enter (accept default).

        Returns:
            Full captured output as string.
        """
        answers = answers or {}
        child = pexpect.spawn(
            self.writ_bin, ["init"],
            cwd=self.cwd,
            timeout=self.timeout,
            encoding="utf-8",
        )

        output_parts: List[str] = []
        try:
            while True:
                idx = child.expect([
                    pexpect.EOF,
                    pexpect.TIMEOUT,
                    r"\[y/N\]",
                    r"\[Y/n\]",
                    r"\?",
                    r":",
                ], timeout=self.timeout)

                output_parts.append(child.before or "")

                if idx in (0, 1):  # EOF or TIMEOUT
                    break

                # Find matching answer from prompt text
                prompt_text = child.before or ""
                answer = ""
                for key, val in answers.items():
                    if key.lower() in prompt_text.lower():
                        answer = val
                        break

                if not answer:
                    answer = "y" if idx in (2, 3) else ""

                child.sendline(answer)
        finally:
            child.close()

        return "\n".join(output_parts)

    def run_interactive(
        self,
        args: List[str],
        interactions: List[Tuple[str, Optional[str]]],
    ) -> str:
        """Run any writ command with scripted interactions.

        Args:
            args: Command arguments (e.g., ["finish"]).
            interactions: List of (expect_pattern, response) tuples.
                         response=None means don't send anything.

        Returns:
            Full captured output as string.
        """
        child = pexpect.spawn(
            self.writ_bin, args,
            cwd=self.cwd,
            timeout=self.timeout,
            encoding="utf-8",
        )

        output: List[str] = []
        try:
            for pattern, response in interactions:
                child.expect(pattern, timeout=self.timeout)
                output.append(child.before or "")
                if response is not None:
                    child.sendline(response)

            child.expect(pexpect.EOF, timeout=self.timeout)
            output.append(child.before or "")
        finally:
            child.close()

        return "\n".join(output)
