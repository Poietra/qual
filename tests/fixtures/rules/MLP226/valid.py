from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        cold = MathTex(f"x = {tracker.get_value():.2f}")
        self.add(cold)
        self.play(FadeIn(cold), run_time=2)
