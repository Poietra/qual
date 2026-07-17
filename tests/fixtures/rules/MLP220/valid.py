from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        bounded = TracedPath(dot.get_center, dissipating_time=0.5)
        short = TracedPath(dot.get_center)
        self.add(dot, bounded, short)
        self.play(dot.animate.shift(RIGHT), run_time=2)
