from manim import *


class Suppressed(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        badge = always_redraw(
            lambda: SVGMobject(f"badge_{tracker.get_value():.0f}.svg")
        )  # qual: ignore[MLP217, MLP201]
        self.add(badge)
        self.play(tracker.animate.set_value(1), run_time=8)
