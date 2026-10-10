"""Literal backend title expectations and explicit legacy-binary selection."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from reliable_mcp.protocol import derive_title, expected_title, matches
from tests.test_real_binary import _binary


class TitleContractCase(unittest.TestCase):
    def test_derived_title_uses_only_nonempty_literal_h1(self):
        cases = (
            ("  # Indented\n# Literal  \nbody", "notes/page.md", "Literal"),
            ("#   \n# Literal\n", "notes/page.md", "Literal"),
            ("# \n  # Indented\n", "notes/page.md", "page"),
            ("## Subheading\nbody", "notes/page.md", "page"),
            ("body\r# Not a new line", "notes/page.md", "page"),
            ("body\u2028# Not a new line", "notes/page.md", "page"),
            ("body", "notes/page", "page"),
            ("body", "notes/page.md.md", "page.md"),
        )
        for body, path, wanted in cases:
            with self.subTest(body=body, path=path):
                self.assertEqual(derive_title(body, path), wanted)

    def test_expected_title_trims_explicit_title_then_falls_back(self):
        for title, wanted in (
            ("Explicit", "Explicit"),
            ("  Explicit  ", "Explicit"),
            ("", "Body title"),
            (" \t ", "Body title"),
            (None, "Body title"),
        ):
            with self.subTest(title=title):
                intent = {"title": title, "body": "# Body title\n", "path": "notes/page.md"}
                self.assertEqual(expected_title(intent), wanted)
        self.assertEqual(expected_title({"body": "# \n", "path": "notes/page.md"}), "page")

    def _intent_and_page(self, raw_title, read_title):
        body = "# Body title  \n\nOriginal body\n"
        intent = {
            "path": "notes/page.md",
            "body": body,
            "title": raw_title,
            "tier": "semantic",
            "pinned": False,
            "tags": ["title-contract"],
            "expires_at": None,
        }
        page = {
            "page_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "body": body,
            "title": read_title,
            "frontmatter": {
                "title": raw_title,
                "tier": "semantic",
                "pinned": False,
                "tags": ["title-contract"],
            },
        }
        return intent, page

    def test_matches_requires_exact_raw_explicit_frontmatter_title(self):
        for raw_title, read_title in (
            ("Explicit", "Explicit"),
            ("  Explicit  ", "Explicit"),
            ("", "Body title"),
            (" \t ", "Body title"),
        ):
            with self.subTest(title=raw_title):
                intent, page = self._intent_and_page(raw_title, read_title)
                self.assertTrue(matches(intent, page))
                del page["frontmatter"]["title"]
                self.assertFalse(matches(intent, page), "missing explicit metadata must not confirm delivery")
                page["frontmatter"]["title"] = None
                self.assertFalse(matches(intent, page))
                page["frontmatter"]["title"] = raw_title + " "
                self.assertFalse(matches(intent, page))

    def test_matches_accepts_missing_frontmatter_title_when_omitted_or_null(self):
        intent, page = self._intent_and_page(None, "Body title")
        del page["frontmatter"]["title"]
        self.assertTrue(matches(intent, page))
        del intent["title"]
        self.assertTrue(matches(intent, page))

    def test_matches_checks_trimmed_read_title_and_exact_original_body(self):
        intent, page = self._intent_and_page("  Explicit  ", "Explicit")
        self.assertTrue(matches(intent, page))
        page["title"] = "  Explicit  "
        self.assertFalse(matches(intent, page))
        page["title"] = "Explicit"
        page["body"] = "# Body title\n\nOriginal body\n"
        self.assertFalse(matches(intent, page))


class LegacyBinarySelectionCase(unittest.TestCase):
    def test_missing_legacy_override_does_not_select_installed_binary(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertIsNone(_binary())

    def test_explicit_legacy_override_is_used_instead_of_current_override(self):
        with tempfile.TemporaryDirectory() as tmp:
            legacy = Path(tmp) / "legacy"
            current = Path(tmp) / "current"
            for path in (legacy, current):
                path.write_text("# synthetic binary selector fixture\n", encoding="utf-8")
                path.chmod(0o700)
            with patch.dict(os.environ, {"AI_MEMORY_LEGACY_BIN": str(legacy), "AI_MEMORY_BIN": str(current)}, clear=True):
                self.assertEqual(_binary(), legacy)

    def test_invalid_legacy_override_does_not_fall_back_to_current_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            current = Path(tmp) / "current"
            current.write_text("# synthetic binary selector fixture\n", encoding="utf-8")
            current.chmod(0o700)
            with patch.dict(os.environ, {"AI_MEMORY_LEGACY_BIN": str(Path(tmp) / "missing"), "AI_MEMORY_BIN": str(current)}, clear=True):
                self.assertIsNone(_binary())


if __name__ == "__main__":
    unittest.main()
