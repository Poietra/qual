from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        badge = always_redraw(lambda: SVGMobject(f"badge_{tracker.get_value():.0f}.svg"))
        label = always_redraw(
            lambda: Text(f"t = {tracker.get_value():.2f}", use_svg_cache=True)
        )
        self.add(badge, label)
        self.play(tracker.animate.set_value(1), run_time=8)
