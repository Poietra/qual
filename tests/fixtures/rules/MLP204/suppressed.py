from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add_updater(lambda dt: self.add(Dot()))  # qual: ignore[MLP204]
        self.add(square)
        self.play(FadeIn(square), run_time=2)
