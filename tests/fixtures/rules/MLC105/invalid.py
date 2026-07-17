from manim import *


def spin(mob, angle):
    return mob.rotate(angle)


class Demo(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda dt: dt)
        square.add_updater(lambda mob, delta: mob)
        square.add_updater(lambda mob, *, dt: mob)
        square.add_updater(spin)
        self.add_updater(lambda mob, dt: mob)
