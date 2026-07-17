"""Smoke fixture: a parseable scene with Japanese text and a suppression."""

from manim import Scene, Square


class デモシーン(Scene):
    def construct(self):
        square = Square()
        self.play(square.animate.shift(2))  # manim-lint: ignore[MLC108]
        self.wait()
