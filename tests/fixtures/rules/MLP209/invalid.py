from manim import RIGHT, Dot, Scene, Square, VGroup


def drift(mob, dt):
    mob.shift(dt * RIGHT)


class Demo(Scene):
    def construct(self):
        background = Square()
        panel = VGroup(Dot(), Dot(), Dot(), Dot(), Dot(), Dot(), Dot(), Dot())
        self.add(background, panel)
        background.add_updater(drift)
        self.wait(2)
