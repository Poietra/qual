from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        disc = always_redraw(lambda: Circle())  # manim-lint: ignore[MLP216]
        self.add(dot, disc)
        self.play(dot.animate.shift(UP), run_time=2)
