from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f"x={tracker.get_value()}"))  # qual: ignore[MLP226]
        self.add(label)
        self.play(tracker.animate.set_value(1), run_time=8)
