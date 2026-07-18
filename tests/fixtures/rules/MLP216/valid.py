from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        ring = always_redraw(lambda: Circle(num_components=14))
        pair = always_redraw(lambda: VGroup(Square(), Square()))
        self.add(dot, ring, pair)
        self.play(dot.animate.shift(UP), run_time=2)
