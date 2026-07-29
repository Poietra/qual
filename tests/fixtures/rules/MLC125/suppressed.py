from manim import *


def spin(mob, dt):
    mob.rotate(dt)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(spin)
        square.remove_updater(lambda m: m)  # qual: ignore[MLC125]
