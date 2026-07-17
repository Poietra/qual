from manim import Scene as Sc, Square as Sq


def spin(mob, dt):
    mob.rotate(dt)


class Demo(Sc):
    def construct(self):
        square = Sq()
        self.add(square)
        square.add_updater(spin)
        square.remove_updater(lambda m: m)
