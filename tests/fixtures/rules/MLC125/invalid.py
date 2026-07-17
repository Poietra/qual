from manim import *


def spin(mob, dt):
    mob.rotate(dt)


def wobble(mob, dt):
    mob.shift(RIGHT)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(spin)
        square.remove_updater(lambda m: m)
        dot = Dot()
        self.add(dot)
        dot.add_updater(spin)
        dot.remove_updater(wobble)
        self.wait(1)
