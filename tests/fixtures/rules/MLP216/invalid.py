from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        disc = always_redraw(lambda: Circle())
        bar = always_redraw(lambda: Line(dot.get_center(), RIGHT * 2))
        self.add(dot, disc, bar)
        self.play(dot.animate.shift(UP), run_time=4)
