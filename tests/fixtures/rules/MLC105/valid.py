from manim import *


def follow(mob):
    return mob


def drift(mob, dt=0.0):
    return mob.shift(dt * RIGHT)


class Demo(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda mob: mob)
        square.add_updater(lambda mob, dt: mob)
        square.add_updater(lambda mob, dt=0: mob)
        square.add_updater(lambda *args: None)
        square.add_updater(follow)
        square.add_updater(drift)
        self.add_updater(lambda dt: dt)
        square.add_updater(make_updater())
        self.add_updater(self.tick)

    def tick(self, dt):
        return dt
