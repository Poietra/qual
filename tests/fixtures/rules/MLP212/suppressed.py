from manim import *


class Suppressed(Scene):
    def construct(self):
        overlay = FullScreenRectangle(fill_opacity=0.4)
        self.play(overlay.animate.scale(1.01), run_time=6)  # manim-lint: ignore[MLP212]
