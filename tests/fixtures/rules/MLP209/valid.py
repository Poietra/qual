from manim import RIGHT, Dot, Scene, Square, VGroup


def drift(mob, dt):
    mob.shift(dt * RIGHT)


class Demo(Scene):
    def construct(self):
        panel = VGroup(Dot(), Dot(), Dot(), Dot(), Dot(), Dot(), Dot(), Dot())
        background = Square()
        self.add(panel, background)
        background.add_updater(drift)
        self.wait(2)
