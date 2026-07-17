from manim import *


def spin(mob, dt):
    mob.rotate(dt)


def get_updater():
    return None


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(get_updater())
        square.remove_updater(spin)
        self.wait(1)
