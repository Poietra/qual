from manim import *


def spin(mob, dt):
    mob.rotate(dt)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(spin)
        square.remove_updater(spin)
        follow = lambda m: m.shift(RIGHT)
        square.add_updater(follow)
        square.remove_updater(follow)
        self.wait(1)
