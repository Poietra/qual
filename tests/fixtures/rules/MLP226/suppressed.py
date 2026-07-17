from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f"x={tracker.get_value()}"))  # manim-lint: ignore[MLP226]
        self.add(label)
        self.play(FadeIn(label), run_time=8)
