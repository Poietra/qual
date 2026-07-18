from manim import *


class Demo(Scene):
    def construct(self):
        ring = always_redraw(lambda: Circle(num_components=14))
        pair = always_redraw(lambda: VGroup(Square(), Square()))
        self.add(ring, pair)
        self.play(FadeIn(ring), run_time=2)
