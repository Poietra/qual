from manim import *


def stale(mob):
    mob.become(Text("frame"))
    return mob


class Demo(Scene):
    def construct(self):
        square = Square()
        callback = stale
        square.add_updater(callback)
        self.add(square)
        self.play(FadeIn(square), run_time=2)
