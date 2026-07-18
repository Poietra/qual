from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex("x = " + str(tracker.get_value())))
        self.add(label)
        self.play(tracker.animate.set_value(1), run_time=8)
