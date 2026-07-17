from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        trace = TracedPath(dot.get_center)
        if dot.submobjects:
            self.add(trace)
        self.add(dot)
        self.play(dot.animate.shift(RIGHT), run_time=8)
