from manim import *


class MobUpdaterScene(Scene):
    def construct(self):
        dot = Dot()
        self.add(dot)
        # A Mobject updater marks its host as moving: the right pattern.
        dot.add_updater(lambda mob, dt: mob.shift(RIGHT * dt))
        # A scene updater that mutates nothing is fine.
        self.add_updater(lambda dt: None)
        self.wait(2)
