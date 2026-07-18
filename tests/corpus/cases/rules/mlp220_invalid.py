from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        trace = TracedPath(dot.get_center)
        self.add(dot, trace)
        self.play(dot.animate.shift(RIGHT), run_time=4)
        self.play(dot.animate.shift(UP), run_time=3)
