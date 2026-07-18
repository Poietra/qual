from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        options = {}
        # A non-literal flag value leaves use_svg_cache unknown.
        unknown_flag = always_redraw(
            lambda: SVGMobject(
                f"badge_{tracker.get_value():.0f}.svg",
                use_svg_cache=tracker.get_value() > 0,
            )
        )
        # A **kwargs splat can hide use_svg_cache=False.
        splat = always_redraw(
            lambda: SVGMobject(f"chip_{tracker.get_value():.0f}.svg", **options)
        )
        self.add(unknown_flag, splat)
        self.play(tracker.animate.set_value(1), run_time=8)
