from manim import *


class SteadyScene(Scene):
    def construct(self):
        dot = Dot()
        driver = Square()
        tracker = ValueTracker(0)
        # Absolute dependency: correct without dt (DESIGN 7.4 prose).
        dot.add_updater(lambda m: m.next_to(driver))
        # dt-scaled relative motion: time-based and FPS-independent.
        dot.add_updater(lambda m, dt: m.shift(dt * RIGHT))
        tracker.add_updater(lambda m, dt: m.increment_value(dt))
        # Tracker-driven step: not a literal coefficient, stays silent.
        dot.add_updater(lambda m: m.shift(tracker.get_value() * RIGHT))
        # Not a relative mutator from the curated list.
        dot.add_updater(lambda m: m.set_opacity(0.5))
        # Scene updaters receive (dt) by contract; different rule territory.
        self.add_updater(lambda dt: dot.shift(dt * RIGHT))
        self.add(dot, driver)
        self.wait(2)
