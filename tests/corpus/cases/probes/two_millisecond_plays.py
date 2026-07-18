from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        square = Square()
        label = always_redraw(lambda: MathTex(f"x = {tracker.get_value():.2f}"))
        self.add(square, label)
        self.play(FadeIn(square), run_time=0.001)
        self.play(FadeIn(square), run_time=0.001)
