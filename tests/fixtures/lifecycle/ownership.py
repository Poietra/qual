from manim import *


def spin(mob, dt):
    mob.rotate(dt)


class Ownership(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(spin)
        self.wait()
        self.add(sq)
        self.wait()
        self.remove(sq)
        self.play(FadeIn(sq))
