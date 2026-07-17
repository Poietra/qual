from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f"x = {tracker.get_value():.2f}"))
        static = always_redraw(lambda: MathTex(r"\pi"))
        self.add(label, static)
        self.play(FadeIn(label), run_time=8)
