from manim import *


class Demo(Scene):
    def construct(self):
        backup = Square()
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        group.add_updater(lambda m: m.copy())
        group.add_updater(lambda m: m.become(backup))
        self.add(group)
        self.play(FadeIn(backup), run_time=2)
