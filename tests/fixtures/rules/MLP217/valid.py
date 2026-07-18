from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        # The fork's Text defaults to use_svg_cache=False: no cache growth.
        plain = always_redraw(lambda: Text(f"t = {tracker.get_value():.2f}"))
        # Cache explicitly disabled on a frame-varying SVG key.
        off = always_redraw(
            lambda: SVGMobject(f"badge_{tracker.get_value():.0f}.svg", use_svg_cache=False)
        )
        # Static key: one cache entry, reused every frame.
        static = always_redraw(lambda: SVGMobject("crest.svg"))
        self.add(plain, off, static)
        self.play(tracker.animate.set_value(1), run_time=8)


class SuspendedHost(Scene):
    def construct(self):
        # Animation.begin() suspends the animated host's updaters for the
        # whole play, and no later play runs the callback: it provably
        # never executes per frame, so no per-frame cache-growth claim
        # survives the liveness gate (and neither MLP201 nor MLP226 fires
        # here either).
        tracker = ValueTracker(0)
        badge = always_redraw(
            lambda: SVGMobject(f"badge_{tracker.get_value():.0f}.svg")
        )
        self.add(badge)
        self.play(FadeIn(badge), run_time=2)
