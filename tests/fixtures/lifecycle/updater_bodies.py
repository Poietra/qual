from manim import *

tracker = ValueTracker(0)


def spin(mob, dt):
    mob.rotate(dt)


def follow(mob):
    mob.move_to(RIGHT * tracker.get_value())


def calls_helper(mob):
    mystery_helper(mob)


class UpdaterBodies(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(spin)
        sq.add_updater(follow)
        sq.add_updater(calls_helper)
        sq.add_updater(lambda m: m.shift(RIGHT))
        self.add(sq)
        self.add_updater(lambda dt: tracker.get_value())
        self.wait()
